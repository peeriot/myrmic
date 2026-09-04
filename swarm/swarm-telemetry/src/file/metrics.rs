use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_sdk::{
    error::OTelSdkResult,
    metrics::{Temporality, data::ResourceMetrics, exporter::PushMetricExporter},
};

impl PushMetricExporter for super::FileExporter {
    /// With cumulative temporality every periodic export carries the current
    /// value of every metric this provider has ever seen, so rewriting the
    /// whole latest-file from one export is complete — the file equivalent of
    /// the db exporter's always-overwritten `metrics_latest` table. No
    /// per-host key (the db path's `zid`) is needed: the file itself is
    /// per-host, and the fetcher attributes rows by which host it read them
    /// from. The db path's full-history `metrics` table has no file
    /// counterpart — nothing reads it.
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
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

        self.rewrite_lines(super::FILE_METRICS_LATEST, entries)
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
