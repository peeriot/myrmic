#![cfg(feature = "export-db")]

mod test_runtime;
mod test_runtime_db;

use std::time::Duration;

use swarm_telemetry::db::opentelemetry_proto::tonic::{
    common::v1::any_value::Value, logs::v1::LogRecord,
};

fn log_body(record: &LogRecord) -> String {
    record
        .body
        .as_ref()
        .and_then(|b| b.value.as_ref())
        .and_then(|v| match v {
            Value::StringValue(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn reload() {
    let run_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    let test_runtime = test_runtime::TestRuntime::spawn("warn", None, Some("1h"), "60s").await;

    let guard = test_runtime
        .spawned
        .telemetry_guard()
        .expect("telemetry guard must be present");

    let before_info = format!("BEFORE-RELOAD-TEST-INFO-{run_id}");
    let before_warn = format!("BEFORE-RELOAD-TEST-WARN-{run_id}");

    tracing::info!("{before_info}");
    tracing::warn!("{before_warn}");

    guard.force_flush().unwrap();

    let bodies = test_runtime
        .await_data::<LogRecord, _, _, _>(
            swarm_telemetry::db::TABLE_LOGS,
            Duration::from_secs(1),
            log_body,
            |l| l == before_warn.as_str(),
        )
        .await;

    assert!(bodies.contains(&before_warn));
    // the info is skipped due to the initial filter level being WARN
    assert!(!bodies.contains(&before_info));

    // change the filter level from WARN to INFO via zenoh publish
    test_runtime
        .spawned
        .session()
        .put(swarm_telemetry::TOPIC_ENV_FILTER, "info")
        .await
        .expect("failed to publish env_filter change");
    // give the subscriber a moment to apply the new filter
    tokio::time::sleep(Duration::from_millis(50)).await;

    let after_info = format!("AFTER-RELOAD-TEST-INFO-{run_id}");
    let after_warn = format!("AFTER-RELOAD-TEST-WARN-{run_id}");

    tracing::info!("{after_info}");
    tracing::warn!("{after_warn}");

    guard.force_flush().unwrap();

    let bodies = test_runtime
        .await_data::<LogRecord, _, _, _>(
            swarm_telemetry::db::TABLE_LOGS,
            Duration::from_secs(1),
            log_body,
            |l| l == after_warn.as_str(),
        )
        .await;

    // now that the filter level is changed to INFO, the info message also must be found
    assert!(bodies.contains(&after_info));
    assert!(bodies.contains(&after_warn));

    test_runtime.shutdown().await;
}
