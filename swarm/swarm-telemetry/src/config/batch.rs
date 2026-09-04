use serde::{Deserialize, Serialize};

/// Overrides for an `OTel` batch processor's defaults (2048 queue / 512 batch, 1s delay for logs,
/// 5s for traces). Every field is optional and, left unset, falls back to the `OTel` SDK's own
/// default (itself overridable via the `OTEL_B{L,S}RP_*` env vars) — so an unconfigured swarm
/// behaves exactly as before this existed.
///
/// The defaults are tuned for typical service telemetry volume, not for a load benchmark that
/// shares its datalayer with the DB-backed exporter: a big periodic export (e.g. up to 512 spans
/// every 5 seconds) is one burst of writes against the same DB the benchmarked cells are using,
/// which can stall unrelated DB-bound work for the duration of that burst. Configuring a shorter
/// delay and/or smaller batch size spreads the same volume over more, smaller bursts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BatchConfig {
    #[serde(default)]
    pub max_queue_size: Option<usize>,
    #[serde(default)]
    pub scheduled_delay_ms: Option<u64>,
    #[serde(default)]
    pub max_export_batch_size: Option<usize>,
}

#[cfg_attr(
    not(any(feature = "export-db", feature = "export-otlp")),
    allow(dead_code)
)]
impl BatchConfig {
    pub(crate) fn apply_to_logs(
        &self,
        builder: opentelemetry_sdk::logs::BatchConfigBuilder,
    ) -> opentelemetry_sdk::logs::BatchConfigBuilder {
        self.apply(builder)
    }

    pub(crate) fn apply_to_traces(
        &self,
        builder: opentelemetry_sdk::trace::BatchConfigBuilder,
    ) -> opentelemetry_sdk::trace::BatchConfigBuilder {
        self.apply(builder)
    }

    fn apply<B: BatchConfigBuilder>(&self, mut builder: B) -> B {
        if let Some(max_queue_size) = self.max_queue_size {
            builder = builder.with_max_queue_size(max_queue_size);
        }
        if let Some(scheduled_delay_ms) = self.scheduled_delay_ms {
            builder =
                builder.with_scheduled_delay(std::time::Duration::from_millis(scheduled_delay_ms));
        }
        if let Some(max_export_batch_size) = self.max_export_batch_size {
            builder = builder.with_max_export_batch_size(max_export_batch_size);
        }
        builder
    }
}

/// The subset of `opentelemetry_sdk::{logs,trace}::BatchConfigBuilder`'s setters that
/// [`BatchConfig::apply`] needs, shared so [`BatchConfig`]'s override logic isn't duplicated per
/// signal type.
#[cfg_attr(
    not(any(feature = "export-db", feature = "export-otlp")),
    allow(dead_code)
)]
trait BatchConfigBuilder {
    fn with_max_queue_size(self, size: usize) -> Self;
    fn with_scheduled_delay(self, delay: std::time::Duration) -> Self;
    fn with_max_export_batch_size(self, size: usize) -> Self;
}

impl BatchConfigBuilder for opentelemetry_sdk::logs::BatchConfigBuilder {
    fn with_max_queue_size(self, size: usize) -> Self {
        self.with_max_queue_size(size)
    }

    fn with_scheduled_delay(self, delay: std::time::Duration) -> Self {
        self.with_scheduled_delay(delay)
    }

    fn with_max_export_batch_size(self, size: usize) -> Self {
        self.with_max_export_batch_size(size)
    }
}

impl BatchConfigBuilder for opentelemetry_sdk::trace::BatchConfigBuilder {
    fn with_max_queue_size(self, size: usize) -> Self {
        self.with_max_queue_size(size)
    }

    fn with_scheduled_delay(self, delay: std::time::Duration) -> Self {
        self.with_scheduled_delay(delay)
    }

    fn with_max_export_batch_size(self, size: usize) -> Self {
        self.with_max_export_batch_size(size)
    }
}
