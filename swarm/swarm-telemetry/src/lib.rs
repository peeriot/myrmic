use std::sync::Arc;

use opentelemetry_sdk::{
    error::OTelSdkResult, logs::SdkLoggerProvider, metrics::SdkMeterProvider,
    trace::SdkTracerProvider,
};

pub mod config;
pub(crate) mod otel;
pub(crate) mod subscribers;
mod trace;

#[cfg(feature = "export-db")]
pub mod db;
#[cfg(any(feature = "export-db", feature = "export-file"))]
pub mod export;
#[cfg(feature = "export-file")]
pub mod file;
#[cfg(feature = "export-db")]
pub mod trace_event_format;

pub use config::TelemetryConfig;
pub use trace::GeneratedTrace;
pub use tracing_subscriber::EnvFilter;
use tracing_subscriber::{Registry, reload};

pub const NO_PARENT_SPAN_ID: u64 = 0;

pub const TOPIC_ENV_FILTER: &str = "@telemetry/@v1/@env-filter";
pub const TOPIC_FORCE_FLUSH: &str = "@telemetry/@v1/@force-flush";

/// Queries all connected nodes for their currently active `EnvFilter` string, returning the
/// distinct values seen (normally just one, since nodes are usually deployed with the same
/// filter). Callers can use this to capture a baseline before overriding it and restore it
/// afterwards.
pub async fn query_env_filter(session: &zenoh::Session) -> Vec<String> {
    use zenoh::query::{ConsolidationMode, QueryTarget};

    let mut filters = Vec::new();
    let Ok(replies) = session
        .get(TOPIC_ENV_FILTER)
        .target(QueryTarget::All)
        .consolidation(ConsolidationMode::None)
        .await
    else {
        return filters;
    };

    while let Ok(reply) = replies.recv_async().await {
        if let Ok(sample) = reply.result()
            && let Ok(value) = std::str::from_utf8(&sample.payload().to_bytes())
            && !filters.iter().any(|f| f == value)
        {
            filters.push(value.to_string());
        }
    }

    filters
}

#[cfg(feature = "export-db")]
pub const TOPIC_DB_RETENTION: &str = "@telemetry/@v1/@db-retention";

/// Reports one telemetry export batch that failed to land (db table or file,
/// entry count).
///
/// This is the one place every exporter's write actually lands
/// (`db::DbExporter::insert_batch` and the `file::FileExporter` writers), which
/// is what makes "the exporter never even tried" distinguishable from "it tried
/// and failed" per table/file. A successful batch says nothing: it happens on
/// every export from every signal, and the point of the file exporter is that
/// telemetry must not compete with the workload.
#[cfg(any(feature = "export-db", feature = "export-file"))]
pub(crate) fn record_insert_batch_outcome<E: std::fmt::Display>(
    table: &str,
    entry_count: usize,
    retention: Option<std::time::Duration>,
    result: Result<&(), E>,
) {
    if let Err(err) = result {
        tracing::warn!(
            "unable to export {entry_count} telemetry entries to {table} \
             (retention {retention:?}): {err}"
        );
    }
}

#[derive(Debug)]
pub struct Guard {
    log_provider: SdkLoggerProvider,
    trace_provider: SdkTracerProvider,
    metric_provider: SdkMeterProvider,
    shutdown_called: std::sync::atomic::AtomicBool,
    reload_handle: reload::Handle<EnvFilter, Registry>,
    abort_filter_reload_handle: tokio::task::AbortHandle,
    abort_filter_query_handle: tokio::task::AbortHandle,
    abort_retention_reload_handle: Option<tokio::task::AbortHandle>,
}

impl Guard {
    pub fn force_flush(&self) -> OTelSdkResult {
        self.metric_provider.force_flush()?;
        self.trace_provider.force_flush()?;
        self.log_provider.force_flush()?;

        Ok(())
    }

    /// Performs a final export and shuts down all providers. Idempotent — safe to call
    /// multiple times and safe to drop after calling (Drop becomes a no-op).
    pub fn shutdown(&self) -> OTelSdkResult {
        let shutdown_called = self
            .shutdown_called
            .swap(true, std::sync::atomic::Ordering::SeqCst);
        if shutdown_called {
            return Ok(());
        }
        self.abort_filter_reload_handle.abort();
        self.abort_filter_query_handle.abort();
        if let Some(handle) = self.abort_retention_reload_handle.as_ref() {
            handle.abort();
        }
        self.metric_provider.shutdown()?;
        self.trace_provider.shutdown()?;
        self.log_provider.shutdown()?;

        Ok(())
    }

    pub fn reload_handle(&self) -> &reload::Handle<EnvFilter, Registry> {
        &self.reload_handle
    }

    pub fn force_flush_queryable(
        self: Arc<Self>,
        session: &zenoh::Session,
    ) -> tokio::task::AbortHandle {
        subscribers::force_flush_queries(session, self)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _unused = self.shutdown();
    }
}
