use anyhow::Context;
use db_client::v1::models::{FieldValue, TxId, tb_insert, ts_publish};
use uuid::Uuid;

const METRICS_LATEST_NS: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x14, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);
use opentelemetry::{KeyValue, Value};
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_sdk::{
    error::{OTelSdkError, OTelSdkResult},
    metrics::{
        Temporality,
        data::{AggregatedMetrics, Gauge, Metric, MetricData, ResourceMetrics, Sum},
        exporter::PushMetricExporter,
    },
};

impl PushMetricExporter for super::DbExporter {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        self.export_otel_table(metrics).await?;

        // Currently we cannot export the data as internal time series in a meaningful manner, the reason
        // is that the internal DB does not store values per set of attributes (tags) while OTel does that.
        //
        // For example, we have two cells, each cell produces the "commands_processed" metric where each
        // cell adds it's SRI as attribute. OpenTelemetry aggregated that count per set of attributes.
        // The internal DB will overwrite the "commands_processed" measure of cell_a with the value of
        // cell_b and vice versa depending on the sequence.
        //
        // self.export_time_series(metrics).await?;

        Ok(())
    }

    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: std::time::Duration) -> OTelSdkResult {
        Ok(())
    }

    fn temporality(&self) -> Temporality {
        Temporality::Cumulative
    }
}

impl super::DbExporter {
    /// Exports the full OpenTelemetry data, that can later be fetched and exported to any compatible collector.
    async fn export_otel_table(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        // Metrics bypass the batched log/trace path, so the "no retention =
        // no db export" gate applies here separately. The retention value itself
        // isn't used further: metrics_latest is always-overwritten state, not
        // individually-expiring rows, so only whether retention is configured matters.
        {
            let read = self.retention.read().await;
            if read.is_none() {
                return Ok(());
            }
        }

        let export = ExportMetricsServiceRequest::from(metrics);
        let entries: Vec<_> = export
            .resource_metrics
            .into_iter()
            .flat_map(|resource_metrics| {
                resource_metrics
                    .scope_metrics
                    .into_iter()
                    .flat_map(|scope_metrics| {
                        let scope_name = scope_metrics.scope.map(|scope| scope.name);
                        scope_metrics
                            .metrics
                            .into_iter()
                            .map(move |metric| (scope_name.clone(), metric))
                    })
            })
            .collect();

        // Keyed per exporting process (`zid`), not just scope+name: every rack host runs its own
        // exporter, and a metric name/scope is commonly shared across hosts (e.g. every tier
        // emits `cell_commands_processed`) — without the zid, each host's periodic export would
        // overwrite the *whole* multi-datapoint row the previous host just wrote, silently
        // dropping every other host's cells from the "latest" snapshot down to whichever host
        // happened to write last. Within one host, this still collapses to that host's single
        // latest sample per metric, which is the intended "latest" semantics (its own repeated
        // periodic exports for the same metric should overwrite each other, not accumulate — the
        // full-history `metrics` table is what accumulates).
        let zid = self.client.zid();

        let entry_count = entries.len();
        let result = self
            .client
            .write_tx_in(super::scope(), async move |client, id| {
                for (scope_name, metric) in entries {
                    let latest_key = format!(
                        "{}:{}:{zid}",
                        scope_name.as_deref().unwrap_or(""),
                        metric.name
                    );
                    let latest_eid = Uuid::new_v5(&METRICS_LATEST_NS, latest_key.as_bytes())
                        .as_bytes()
                        .to_vec();

                    let scoped_entry = super::ScopedEntry {
                        scope_name,
                        data: &metric,
                    };
                    let value =
                        serde_json::to_vec(&scoped_entry).context("Failed to serialize entry")?;

                    client
                        .send(tb_insert::Request {
                            id,
                            op: tb_insert::Op {
                                scope: super::scope(),
                                table: super::TABLE_METRICS.into(),
                                eid: None,
                                value: value.clone(),
                            },
                        })
                        .await?
                        .map_err(|err| anyhow::anyhow!("{}", err.message))?;

                    // this latest table is a hack to easily grep the latest values without scanning the full table. it might
                    // be useless when we can use time series database that allow unique values per attribute set.
                    client
                        .send(tb_insert::Request {
                            id,
                            op: tb_insert::Op {
                                scope: super::scope(),
                                table: super::TABLE_METRICS_LATEST.into(),
                                eid: Some(latest_eid),
                                value,
                            },
                        })
                        .await?
                        .map_err(|err| anyhow::anyhow!("{}", err.message))?;
                }
                Ok(())
            })
            .await;

        crate::record_insert_batch_outcome(
            "metrics+metrics_latest",
            entry_count,
            None,
            result.as_ref(),
        );
        result.map_err(|err| OTelSdkError::InternalFailure(err.to_string()))
    }

    /// Converts the OpenTelemetry data into internal time series.
    #[expect(unused, reason = "DB support missing")]
    async fn export_time_series(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        let metrics_iter = metrics
            .scope_metrics()
            .flat_map(|scope_metrics| {
                let scope_name = scope_metrics.scope().name();
                scope_metrics
                    .metrics()
                    .map(move |metric| (scope_name, metric))
            })
            .collect::<Vec<_>>();

        self.client
            .write_tx_in(super::scope(), async move |client, id| {
                for (_, metric) in metrics_iter {
                    let ts_requests = metric.as_ts_requests(id, metric.name());

                    for req in ts_requests {
                        client
                            .send(req)
                            .await?
                            .map_err(|err| anyhow::anyhow!("{}", err.message))?;
                    }
                }

                Ok(())
            })
            .await
            .map_err(|err| OTelSdkError::InternalFailure(err.to_string()))
    }
}

fn build_ts_request<'a, T>(
    id: TxId,
    name: &str,
    attributes: impl Iterator<Item = &'a KeyValue>,
    value: T,
    timestamp: u64,
) -> ts_publish::Request
where
    T: Copy,
    FieldValue: From<T>,
{
    ts_publish::Request {
        id,
        op: ts_publish::Op {
            scope: super::scope(),
            measurement: name.into(),
            tags: attributes
                .filter_map(|kv| {
                    if let Value::String(value) = &kv.value {
                        Some((kv.key.as_str().to_owned(), value.as_str().to_owned()))
                    } else {
                        None
                    }
                })
                .collect(),
            fields: vec![(String::from("value"), FieldValue::from(value))],
            timestamp,
        },
    }
}

trait AsTsRequests {
    fn as_ts_requests(&self, id: TxId, name: &str) -> Vec<ts_publish::Request>;
}

impl AsTsRequests for Metric {
    fn as_ts_requests(&self, id: TxId, name: &str) -> Vec<ts_publish::Request> {
        match self.data() {
            AggregatedMetrics::F64(metric_data) => metric_data.as_ts_requests(id, name),
            AggregatedMetrics::U64(metric_data) => metric_data.as_ts_requests(id, name),
            AggregatedMetrics::I64(metric_data) => metric_data.as_ts_requests(id, name),
        }
    }
}

impl<T> AsTsRequests for MetricData<T>
where
    T: Copy,
    FieldValue: From<T>,
{
    fn as_ts_requests(&self, id: TxId, name: &str) -> Vec<ts_publish::Request> {
        match self {
            MetricData::Gauge(gauge) => gauge.as_ts_requests(id, name),
            MetricData::Sum(sum) => sum.as_ts_requests(id, name),
            MetricData::Histogram(_) | MetricData::ExponentialHistogram(_) => vec![],
        }
    }
}

impl<T> AsTsRequests for Gauge<T>
where
    T: Copy,
    FieldValue: From<T>,
{
    #[expect(
        clippy::expect_used,
        reason = "we can survive until year 2554 casting nano seconds since UNIX_EPOCH into u64"
    )]
    // @jezza is aware of that problem and will provide a solution before 2554
    fn as_ts_requests(&self, id: TxId, name: &str) -> Vec<ts_publish::Request> {
        let timestamp = super::duration_since_unix_epoch(self.time())
            .as_nanos()
            .try_into()
            .expect("can cast nano seconds since UNIX_EPOCH into u64");
        self.data_points()
            .map(|dp| build_ts_request(id, name, dp.attributes(), dp.value(), timestamp))
            .collect()
    }
}

impl<T> AsTsRequests for Sum<T>
where
    T: Copy,
    FieldValue: From<T>,
{
    #[expect(
        clippy::expect_used,
        reason = "we can survive until year 2554 casting nano seconds since UNIX_EPOCH into u64"
    )]
    // @jezza is aware of that problem and will provide a solution before 2554
    fn as_ts_requests(&self, id: TxId, name: &str) -> Vec<ts_publish::Request> {
        let timestamp = super::duration_since_unix_epoch(self.time())
            .as_nanos()
            .try_into()
            .expect("can cast nano seconds since UNIX_EPOCH into u64");
        self.data_points()
            .map(|dp| build_ts_request(id, name, dp.attributes(), dp.value(), timestamp))
            .collect()
    }
}
