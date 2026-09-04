use opentelemetry_sdk::trace::SdkTracerProvider;
#[cfg(any(
    feature = "export-db",
    feature = "export-file",
    feature = "export-otlp"
))]
use opentelemetry_sdk::trace::{BatchConfigBuilder, BatchSpanProcessor};

impl super::OtelProvider for crate::config::traces::Config {
    type Provider = SdkTracerProvider;

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
        let mut builder = SdkTracerProvider::builder().with_resource(resource);

        #[cfg(feature = "export-db")]
        {
            let batch_config = self
                .batch
                .apply_to_traces(BatchConfigBuilder::default())
                .build();
            let processor = BatchSpanProcessor::builder(exporters.db.clone())
                .with_batch_config(batch_config)
                .build();
            builder = builder.with_span_processor(processor);
        }

        #[cfg(feature = "export-file")]
        if let Some(file_exporter) = exporters.file.clone() {
            let batch_config = self
                .batch
                .apply_to_traces(BatchConfigBuilder::default())
                .build();
            let processor = BatchSpanProcessor::builder(file_exporter)
                .with_batch_config(batch_config)
                .build();
            builder = builder.with_span_processor(processor);
        }

        #[cfg(feature = "export-otlp")]
        {
            use opentelemetry_otlp::WithExportConfig;

            if let Some(endpoint) = self.otel_endpoint.as_ref()
                && let Ok(exporter) = opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .build()
                    .inspect_err(|err| {
                        tracing::error!(
                            "Failed to build SpanExporter ({err}). Skipping telemetry layer."
                        );
                    })
            {
                let batch_config = self
                    .batch
                    .apply_to_traces(BatchConfigBuilder::default())
                    .build();
                let processor = BatchSpanProcessor::builder(exporter)
                    .with_batch_config(batch_config)
                    .build();
                builder = builder.with_span_processor(processor);
            }
        }

        builder.build()
    }
}
