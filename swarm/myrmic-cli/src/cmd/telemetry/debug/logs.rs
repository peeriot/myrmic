use std::time::{Duration, SystemTime, UNIX_EPOCH};

use db_client::v1::Subscription;
use db_commons::models::{Cursor, Subject, events, tb_list};
use swarm_telemetry::db::opentelemetry_proto::tonic::common::v1::any_value::Value;
use swarm_telemetry::db::opentelemetry_proto::tonic::logs::v1::LogRecord;
use swarm_telemetry::db::{ScopedEntry, TABLE_LOGS};

/// The tracing targets that carry cell log output — the WASM host-function logger on edge
/// devices, and the hardcoded target the host re-emits embedded-cell logs under. `debug` only
/// cares about cell logs, so it filters everything else out client-side rather than relying on
/// the remote `EnvFilter` (which also gates `OTel` export for the whole process).
const CELL_LOG_TARGETS: &[&str] = &[
    "swarm::embedded",
    "sorg_execution::wasm::host_functions::logging",
];

pub(crate) fn is_cell_target(target: Option<&str>) -> bool {
    target.is_some_and(|target| CELL_LOG_TARGETS.contains(&target))
}

/// Builds the `EnvFilter` directives that raise `CELL_LOG_TARGETS` to `level`, to be appended
/// to whatever filter is already active.
pub(crate) fn level_override_directives(level: &str) -> String {
    CELL_LOG_TARGETS
        .iter()
        .map(|target| format!("{target}={level}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) struct LogSubscriber {
    _subscription: Subscription,
}

impl LogSubscriber {
    pub(crate) async fn new(
        db: db_client::v1::Client,
        tx: tokio::sync::mpsc::Sender<()>,
    ) -> anyhow::Result<Self> {
        let tele_scope = swarm_telemetry::db::scope();

        let subscription = db
            .subscribe(
                Subject::Database(tele_scope.namespace, tele_scope.database),
                TABLE_LOGS,
                move |event| {
                    tokio::spawn(notification_handler(event, tx.clone()));
                },
            )
            .await
            .map_err(|err| anyhow::anyhow!("Failed to subscribe: {err}"))?;

        Ok(Self {
            _subscription: subscription,
        })
    }
}

async fn notification_handler(
    _notification: events::Notification,
    sender: tokio::sync::mpsc::Sender<()>,
) {
    // we are just interested in the fact that a new log batch has been inserted
    if let Err(err) = sender.send(()).await {
        eprintln!("{err}");
    }
}

pub(crate) async fn query(
    db: &db_client::v1::Client,
    cursor: Option<Cursor>,
) -> anyhow::Result<tb_list::Response> {
    db.read_tx_in(swarm_telemetry::db::scope(), async move |client, tx_id| {
        let req = tb_list::Request {
            id: tx_id,
            op: tb_list::Op {
                scope: swarm_telemetry::db::scope(),
                table: TABLE_LOGS.into(),
                cursor,
                limit: None,
                order: None,
            },
        };

        Ok(client
            .send(req)
            .await?
            .map_err(|err| anyhow::anyhow!("{}", err.message))?)
    })
    .await
    .map_err(|err| anyhow::anyhow!("{err}"))
}

/// Parses a raw row payload into the `LogRecord` stored by the telemetry
/// exporter (JSON, not postcard — see `swarm_telemetry::db::DbExporter`),
/// along with its `scope_name` (the tracing `target`, e.g. a module path) —
/// the OTLP `LogRecord` proto has no `target` field of its own, so the
/// exporter carries it alongside the record instead.
pub(crate) fn parse(payload: &[u8]) -> Option<(Option<String>, LogRecord)> {
    serde_json::from_slice::<ScopedEntry<LogRecord>>(payload)
        .inspect_err(|err| eprintln!("Failed to parse log record: {err}"))
        .ok()
        .map(|entry| (entry.scope_name, entry.data))
}

/// The record's own emission time (falls back to the observed time if the
/// original timestamp wasn't set), used to decide when it's safe to flush a
/// queued `DebugItem` — not to be confused with the row's insertion time.
pub(crate) fn time(record: &LogRecord) -> SystemTime {
    let ts_ns = record.observed_time_unix_nano.max(record.time_unix_nano);
    UNIX_EPOCH + Duration::from_nanos(ts_ns)
}

/// Find the records sri attribute if it exists
pub(crate) fn sri(record: &LogRecord) -> Option<&String> {
    for kv in &record.attributes {
        if kv.key == "sri" {
            let value = kv.value.as_ref()?.value.as_ref()?;
            match value {
                Value::StringValue(sri) => return Some(sri),
                _ => return None,
            }
        }
    }

    None
}

/// Renders a log record the same way as `myrmic telemetry logs`, minus the
/// trace ID (already shown on the `DebugItem` it's grouped under).
pub(crate) fn format(record: &LogRecord) -> String {
    let message = record
        .body
        .as_ref()
        .and_then(|v| v.value.as_ref())
        .and_then(|v| match v {
            Value::StringValue(v) => Some(v.as_str()),
            _ => None,
        })
        .unwrap_or("");

    let level = match record.severity_number {
        1..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        _ => "UNKNOWN",
    };

    format!(
        "[{}] {level} {message}",
        humantime::format_rfc3339(time(record))
    )
}
