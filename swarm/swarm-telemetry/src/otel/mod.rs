use opentelemetry_sdk::Resource;

mod logs;
mod metrics;
mod traces;

/// The exporter backends compiled into this build, constructed once in
/// [`TelemetryConfig::try_global`](crate::TelemetryConfig::try_global) and
/// handed to every provider builder — each provider clones the ones it wires
/// in. With no export feature enabled this is empty and the providers build
/// bare (plus whatever `export-otlp` adds from the config's own endpoints).
pub(crate) struct Exporters {
    #[cfg(feature = "export-db")]
    pub db: crate::db::DbExporter,
    /// `None` when the config sets no `file_export_dir` (or the directory
    /// could not be created).
    #[cfg(feature = "export-file")]
    pub file: Option<crate::file::FileExporter>,
}

pub(crate) trait OtelProvider {
    type Provider;

    fn build_provider(&self, resource: Resource, exporters: &Exporters) -> Self::Provider;
}
