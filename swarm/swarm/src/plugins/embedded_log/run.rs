use swarm_api::DropNotifier;
use swarm_telemetry_embedded::{Level, TelemetryRecord};
use zenoh::Session;

pub(super) async fn run(session: Session, drop_rx: DropNotifier) {
    tracing::debug!("spawning embedded-log transformer");

    let worker = tokio::spawn(worker(session));
    let abort_handle = worker.abort_handle();

    tokio::select! {
        _ = drop_rx.recv_async() => {
            abort_handle.abort();
            tracing::info!("shutting down embedded-log transformer");
        }
        _ = worker => {
            tracing::warn!("embedded-log transformer exited unexpectedly");
        }
    }
}

async fn worker(session: Session) {
    if let Ok(subscriber) = session
        .declare_subscriber(swarm_telemetry_embedded::TOPIC_LOGS)
        .await
        .inspect_err(|err| tracing::warn!("Failed to declare embedded log subscriber: {err}"))
    {
        while let Ok(event) = subscriber.recv_async().await {
            let bytes = event.payload().to_bytes();
            let record = match postcard::from_bytes::<TelemetryRecord>(&bytes) {
                Ok(record) => record,
                Err(err) => {
                    tracing::warn!("received invalid embedded log record: {err}");
                    continue;
                }
            };

            let target = record.target.as_str();
            let msg = record.message.as_str();

            // re-emit as a tracing event; the configured subscriber handles OTel/DB export
            match record.level {
                Level::Error => {
                    tracing::error!(target: "swarm::embedded", embedded_target = target, "{msg}");
                }
                Level::Warn => {
                    tracing::warn!(target: "swarm::embedded", embedded_target = target, "{msg}");
                }
                Level::Info => {
                    tracing::info!(target: "swarm::embedded", embedded_target = target, "{msg}");
                }
                Level::Debug => {
                    tracing::debug!(target: "swarm::embedded", embedded_target = target, "{msg}");
                }
                Level::Trace => {
                    tracing::trace!(target: "swarm::embedded", embedded_target = target, "{msg}");
                }
            }
        }
    }
}
