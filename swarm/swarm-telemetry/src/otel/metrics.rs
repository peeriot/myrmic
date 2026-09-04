use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};

impl super::OtelProvider for crate::config::metrics::Config {
    type Provider = SdkMeterProvider;

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
        let mut builder = SdkMeterProvider::builder().with_resource(resource);

        #[cfg(feature = "export-db")]
        {
            builder = builder.with_reader(self.periodic_reader(exporters.db.clone()));
        }

        #[cfg(feature = "export-file")]
        if let Some(file_exporter) = exporters.file.clone() {
            builder = builder.with_reader(self.periodic_reader(file_exporter));
        }

        #[cfg(feature = "export-otlp")]
        {
            use opentelemetry_otlp::WithExportConfig;

            if let Some(endpoint) = self.otel_endpoint.as_ref()
                && let Ok(exporter) = opentelemetry_otlp::MetricExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .build()
                    .inspect_err(|err| {
                        tracing::error!(
                            "Failed to build MetricExporter ({err}). Skipping telemetry layer."
                        );
                    })
            {
                builder = builder.with_reader(self.periodic_reader(exporter));
            }
        }

        builder.build()
    }
}

impl crate::config::metrics::Config {
    #[cfg_attr(
        not(any(
            feature = "export-db",
            feature = "export-file",
            feature = "export-otlp"
        )),
        allow(dead_code)
    )]
    fn periodic_reader<E: opentelemetry_sdk::metrics::exporter::PushMetricExporter>(
        &self,
        exporter: E,
    ) -> PeriodicReader<E> {
        let mut builder = PeriodicReader::builder(exporter);
        if let Some(export_interval_ms) = self.export_interval_ms {
            builder = builder.with_interval(std::time::Duration::from_millis(export_interval_ms));
        }
        builder.build()
    }
}
