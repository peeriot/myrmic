use std::time::UNIX_EPOCH;

use chrono::{DateTime, Local, SecondsFormat, Utc};
use swarm_telemetry::db::opentelemetry_proto::tonic::common::v1::any_value::Value;
use swarm_telemetry::db::opentelemetry_proto::tonic::logs::v1::LogRecord;
use uuid::Uuid;

use crate::args::Ctx;

pub async fn handle(
    ctx: Ctx,
    cmd: super::Logs,
    db_client: db_client::v1::Client,
) -> anyhow::Result<()> {
    let entities =
        super::query_telemetry_data::<LogRecord>(db_client, swarm_telemetry::db::TABLE_LOGS)
            .await?;

    for (_id, scoped_log) in entities {
        let trace_id = scoped_log
            .data
            .trace_id
            .try_into()
            .map(u128::from_be_bytes)
            .map(Uuid::from_u128)
            .ok();

        if cmd
            .trace_id_filter
            .trace_id
            .is_some_and(|f| trace_id != Some(f))
        {
            continue;
        }

        let message = scoped_log
            .data
            .body
            .as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| {
                if let Value::StringValue(v) = v {
                    Some(v.as_str())
                } else {
                    None
                }
            })
            .unwrap_or("");

        let trace_id_block = match trace_id {
            Some(trace_id) => format!("| trace_id = {} | ", trace_id.as_simple()),
            None => String::new(),
        };

        let ts_ns = scoped_log
            .data
            .observed_time_unix_nano
            .max(scoped_log.data.time_unix_nano);
        let duration = std::time::Duration::from_nanos(ts_ns);
        let time = DateTime::<Utc>::from(UNIX_EPOCH + duration)
            .with_timezone(&Local)
            .to_rfc3339_opts(SecondsFormat::Millis, false);

        match scoped_log.data.severity_number {
            (1..=4) => crate::trace!(ctx, "[{}] {}{}", time, trace_id_block, message),
            (5..=8) => crate::debug!(ctx, "[{}] {}{}", time, trace_id_block, message),
            (9..=12) => crate::info!(ctx, "[{}] {}{}", time, trace_id_block, message),
            (13..=16) => crate::warn!(ctx, "[{}] {}{}", time, trace_id_block, message),
            (17..=20) => crate::error!(ctx, "[{}] {}{}", time, trace_id_block, message),
            _ => {}
        }
    }

    Ok(())
}
