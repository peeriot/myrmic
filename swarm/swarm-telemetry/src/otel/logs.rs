use opentelemetry_sdk::logs::SdkLoggerProvider;
#[cfg(any(
    feature = "export-db",
    feature = "export-file",
    feature = "export-otlp"
))]
use opentelemetry_sdk::logs::{BatchConfigBuilder, BatchLogProcessor};

impl super::OtelProvider for crate::config::logs::Config {
    type Provider = SdkLoggerProvider;

    #[cfg_attr(
        not(any(
            feature = "export-db",
            feature = "export-file",
            feature = "export-otlp"
        )),
        allow(unused_mut, unused_variables)
    )]
    fn build_provider(
        &self,
        resource: opentelemetry_sdk::Resource,
        exporters: &super::Exporters,
    ) -> Self::Provider {
        let mut builder = SdkLoggerProvider::builder().with_resource(resource);

        #[cfg(feature = "export-db")]
        {
            let batch_config = self
                .batch
                .apply_to_logs(BatchConfigBuilder::default())
                .build();
            let processor = BatchLogProcessor::builder(exporters.db.clone())
                .with_batch_config(batch_config)
                .build();
            builder = builder.with_log_processor(processor);
        }

        #[cfg(feature = "export-file")]
        if let Some(file_exporter) = exporters.file.clone() {
            let batch_config = self
                .batch
                .apply_to_logs(BatchConfigBuilder::default())
                .build();
            let processor = BatchLogProcessor::builder(file_exporter)
                .with_batch_config(batch_config)
                .build();
            builder = builder.with_log_processor(processor);
        }

        #[cfg(feature = "export-otlp")]
        {
            use opentelemetry_otlp::WithExportConfig;

            if let Some(endpoint) = self.otel_endpoint.as_ref()
                && let Ok(exporter) = opentelemetry_otlp::LogExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .build()
                    .inspect_err(|err| {
                        tracing::error!(
                            "Failed to build LogExporter ({err}). Skipping telemetry layer."
                        );
                    })
            {
                let batch_config = self
                    .batch
                    .apply_to_logs(BatchConfigBuilder::default())
                    .build();
                let processor = BatchLogProcessor::builder(exporter)
                    .with_batch_config(batch_config)
                    .build();
                builder = builder.with_log_processor(processor);
            }
        }

        builder.build()
    }
}
