use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::Resource;
use serde::{Deserialize, Serialize};
use tracing_subscriber::{
    layer::SubscriberExt,
    util::{SubscriberInitExt, TryInitError},
};

pub mod batch;
pub mod logs;
pub mod metrics;
pub mod traces;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub logs: logs::Config,
    #[serde(default)]
    pub metrics: metrics::Config,
    #[serde(default)]
    pub traces: traces::Config,
    #[serde(default, with = "serde_humantime")]
    pub db_retention: Option<humantime::Duration>,
    /// When set (and the `export-file` feature is compiled in), every signal is
    /// additionally exported to JSON-lines files under this directory — see
    /// [`crate::file`]. Independent of `db_retention`: a deployment that must
    /// keep telemetry off the shared db (a load benchmark) sets only this.
    #[serde(default)]
    pub file_export_dir: Option<std::path::PathBuf>,
}

mod serde_humantime {
    use serde::Deserialize;

    #[expect(
        clippy::ref_option,
        reason = "serde expects &Option<humantime::Duration>"
    )]
    pub fn serialize<S>(value: &Option<humantime::Duration>, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match value.as_ref() {
            Some(value) => s.serialize_some(&value.to_string()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Option<humantime::Duration>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(d)?;

        value
            .map(|s| humantime::parse_duration(&s).map(humantime::Duration::from))
            .transpose()
            .map_err(serde::de::Error::custom)
    }
}

impl TelemetryConfig {
    #[must_use]
    pub fn has_global() -> bool {
        tracing_core::dispatcher::has_been_set()
    }

    /// tries to install a global tracing subscriber
    pub fn try_global(
        &self,
        service_name: String,
        session: &zenoh::Session,
    ) -> Result<crate::Guard, TryInitError> {
        use crate::otel::OtelProvider;

        let resource = Resource::builder()
            .with_service_name(service_name.clone())
            .build();

        #[cfg(feature = "export-db")]
        let (db_exporter, abort_retention_reload_handle) = {
            let client = db_client::v1::Client::new(session);
            let db_exporter = crate::db::DbExporter::new(client, self.db_retention.map(Into::into));

            let abort_retention_reload_handle = crate::subscribers::db_retention_reload_events(
                session,
                db_exporter.retention_lock(),
            );

            (db_exporter, Some(abort_retention_reload_handle))
        };

        #[cfg(not(feature = "export-db"))]
        let abort_retention_reload_handle: Option<tokio::task::AbortHandle> = None;

        // The subscriber isn't installed yet, so tracing would drop a warning
        // here — eprintln, like `logs::Config::file_layer`.
        #[cfg(feature = "export-file")]
        let file_exporter = self.file_export_dir.as_ref().and_then(|dir| {
            crate::file::FileExporter::new(dir.clone())
                .inspect_err(|err| {
                    eprintln!(
                        "failed to open telemetry file-export directory {}: {err}; file export \
                         disabled",
                        dir.display()
                    );
                })
                .ok()
        });

        let exporters = crate::otel::Exporters {
            #[cfg(feature = "export-db")]
            db: db_exporter,
            #[cfg(feature = "export-file")]
            file: file_exporter,
        };

        let log_provider = self.logs.build_provider(resource.clone(), &exporters);
        let trace_provider = self.traces.build_provider(resource.clone(), &exporters);
        let metric_provider = self.metrics.build_provider(resource, &exporters);

        opentelemetry::global::set_meter_provider(metric_provider.clone());

        let (env_filter_layer, reload_handle) =
            tracing_subscriber::reload::Layer::new(self.logs.env_filter());

        tracing_subscriber::registry()
            .with(env_filter_layer)
            .with(self.logs.fmt_layer())
            .with(self.logs.file_layer())
            .with(
                opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(
                    &log_provider,
                ),
            )
            .with(tracing_opentelemetry::layer().with_tracer(trace_provider.tracer(service_name)))
            .try_init()?;

        let abort_filter_reload_handle =
            crate::subscribers::filter_reload_events(session, reload_handle.clone());
        let abort_filter_query_handle =
            crate::subscribers::filter_query_events(session, reload_handle.clone());

        Ok(crate::Guard {
            log_provider,
            trace_provider,
            metric_provider,
            shutdown_called: std::sync::atomic::AtomicBool::new(false),
            reload_handle,
            abort_filter_reload_handle,
            abort_filter_query_handle,
            abort_retention_reload_handle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{TelemetryConfig, logs::Format, metrics, traces};

    #[test]
    fn deserialize_defaults_from_empty_object() {
        let config: TelemetryConfig = serde_json::from_str("{}").unwrap();

        assert_eq!(config.logs.format, Format::Full);
        assert_eq!(config.logs.env_filter, None);
        assert_eq!(config.logs.otel_endpoint, None);
        assert_eq!(config.metrics.otel_endpoint, None);
        assert_eq!(config.traces.otel_endpoint, None);
    }

    #[test]
    fn serialize_log_format_uses_screaming_snake_case() {
        let encoded = serde_json::to_string(&Format::Pretty).unwrap();

        assert_eq!(encoded, "\"PRETTY\"");
    }

    #[test]
    fn telemetry_config_round_trips_through_json() {
        let config = TelemetryConfig {
            logs: super::logs::Config {
                format: Format::Json,
                env_filter: Some(String::from("INFO")),
                otel_endpoint: Some(String::from("http://logs:4317")),
                ..Default::default()
            },
            metrics: metrics::Config {
                otel_endpoint: Some(String::from("http://metrics:4317")),
                export_interval_ms: None,
            },
            traces: traces::Config {
                otel_endpoint: Some(String::from("http://traces:4317")),
                batch: Default::default(),
            },
            db_retention: None,
            file_export_dir: None,
        };

        let encoded = serde_json::to_string(&config).unwrap();
        let decoded: TelemetryConfig = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.logs.format, Format::Json);
        assert_eq!(decoded.logs.env_filter.as_deref(), Some("INFO"));
        assert_eq!(
            decoded.logs.otel_endpoint.as_deref(),
            Some("http://logs:4317")
        );
        assert_eq!(
            decoded.metrics.otel_endpoint.as_deref(),
            Some("http://metrics:4317")
        );
        assert_eq!(
            decoded.traces.otel_endpoint.as_deref(),
            Some("http://traces:4317")
        );
    }
}
