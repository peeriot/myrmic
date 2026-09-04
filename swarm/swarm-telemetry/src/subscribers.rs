use std::sync::Arc;

#[cfg(feature = "export-db")]
use tokio::sync::RwLock;
use tracing_subscriber::{EnvFilter, Registry, reload};

pub(crate) fn filter_reload_events(
    session: &zenoh::Session,
    reload_handle: reload::Handle<EnvFilter, Registry>,
) -> tokio::task::AbortHandle {
    let session = session.clone();
    tokio::task::spawn(async move {
        let Ok(subscriber) = session.declare_subscriber(crate::TOPIC_ENV_FILTER).await else {
            tracing::warn!(
                "failed to declare env_filter reload subscriber on {}",
                crate::TOPIC_ENV_FILTER
            );
            return;
        };
        while let Ok(sample) = subscriber.recv_async().await {
            let bytes = sample.payload().to_bytes();
            if let Ok(filter_str) = std::str::from_utf8(&bytes) {
                let _ = reload_handle
                    .modify(|f| match EnvFilter::try_new(filter_str) {
                        Ok(new_filter) => *f = new_filter,
                        Err(e) => tracing::warn!("invalid env_filter '{filter_str}': {e}"),
                    })
                    .inspect_err(|e| tracing::warn!("failed to reload env_filter: {e}"));

                tracing::info!("env_filter set to {filter_str}");
            }
        }
    })
    .abort_handle()
}

/// Answers queries on `TOPIC_ENV_FILTER` with the currently active filter string, so a
/// remote caller can learn the baseline before overriding it (and restore it afterwards).
pub(crate) fn filter_query_events(
    session: &zenoh::Session,
    reload_handle: reload::Handle<EnvFilter, Registry>,
) -> tokio::task::AbortHandle {
    let session = session.clone();
    tokio::task::spawn(async move {
        let Ok(queryable) = session.declare_queryable(crate::TOPIC_ENV_FILTER).await else {
            tracing::warn!(
                "failed to declare env_filter query handler on {}",
                crate::TOPIC_ENV_FILTER
            );
            return;
        };
        while let Ok(query) = queryable.recv_async().await {
            let reply_result = match reload_handle.with_current(ToString::to_string) {
                Ok(current) => query.reply(query.key_expr(), current).await,
                Err(err) => {
                    query
                        .reply_err(format!("failed to read current env_filter: {err}"))
                        .await
                }
            };
            if let Err(err) = reply_result {
                tracing::warn!("failed to reply to env_filter query: {err}");
            }
        }
    })
    .abort_handle()
}

pub(crate) fn force_flush_queries(
    session: &zenoh::Session,
    guard: Arc<crate::Guard>,
) -> tokio::task::AbortHandle {
    let session = session.clone();
    tokio::task::spawn(async move {
        let Ok(queryable) = session.declare_queryable(crate::TOPIC_FORCE_FLUSH).await else {
            tracing::warn!(
                "failed to declare telemetry force-flush queryable on {}",
                crate::TOPIC_FORCE_FLUSH
            );
            return;
        };

        // Which process failed matters as much as why: the force-flush caller fans out to every
        // matching queryable (`QueryTarget::All`, see `test-framework`'s `force_flush_telemetry`),
        // and an unlabeled error reply leaves "which host?" to per-node forensics on exactly the
        // deployment where this process's own logs are discarded (daemonized rack runtimes).
        // /etc/hostname names the machine on those deployments; the session zid is the fallback
        // that always exists and is already how the DB keys per-host metrics.
        let host = std::fs::read_to_string("/etc/hostname")
            .map(|s| s.trim().to_owned())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| session.zid().to_string());

        while let Ok(query) = queryable.recv_async().await {
            // `Guard::force_flush` is a synchronous call that blocks the calling thread until
            // the exporter's flush completes — which itself needs this same runtime's worker
            // threads free to drive the underlying DB write's own zenoh I/O. Running it inline
            // here would tie up one of those worker threads for the flush's whole duration,
            // competing with the very I/O it's waiting on; `spawn_blocking` moves it off the
            // async worker pool entirely so it can never starve (or be starved by) the runtime.
            let guard = guard.clone();
            let flush_result = tokio::task::spawn_blocking(move || guard.force_flush())
                .await
                .unwrap_or_else(|err| {
                    Err(opentelemetry_sdk::error::OTelSdkError::InternalFailure(
                        format!("force-flush task panicked: {err}"),
                    ))
                });

            match flush_result {
                Ok(()) => {
                    if let Err(err) = query.reply(query.key_expr(), b"ok".to_vec()).await {
                        tracing::warn!("failed to reply to telemetry force-flush query: {err}");
                    }
                }
                Err(err) => {
                    if let Err(reply_err) = query
                        .reply_err(format!("[{host}] failed to force flush telemetry: {err}"))
                        .await
                    {
                        tracing::warn!(
                            "failed to reply error to telemetry force-flush query: {reply_err}"
                        );
                    }
                }
            }
        }
    })
    .abort_handle()
}

#[cfg(feature = "export-db")]
pub(crate) fn db_retention_reload_events(
    session: &zenoh::Session,
    retention_lock: Arc<RwLock<Option<std::time::Duration>>>,
) -> tokio::task::AbortHandle {
    let session = session.clone();
    tokio::task::spawn(async move {
        let Ok(subscriber) = session.declare_subscriber(crate::TOPIC_DB_RETENTION).await else {
            tracing::warn!(
                "failed to declare DB retention reload subscriber on {}",
                crate::TOPIC_DB_RETENTION
            );
            return;
        };
        while let Ok(sample) = subscriber.recv_async().await {
            let bytes = sample.payload().to_bytes();

            if let Ok(retention_str) = std::str::from_utf8(&bytes) {
                let mut guard = retention_lock.write().await;
                if retention_str == "null" {
                    *guard = None;
                } else if let Ok(retention) = humantime::parse_duration(retention_str) {
                    *guard = Some(retention);
                }
                tracing::info!("DB retention set to {retention_str}");
            }
        }
    })
    .abort_handle()
}
