#![cfg(feature = "export-db")]

mod test_runtime;
mod test_runtime_db;

use std::time::Duration;

use swarm_telemetry::db::opentelemetry_proto::tonic::{
    common::v1::any_value::Value, logs::v1::LogRecord, metrics::v1::Metric,
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
async fn db_export_follows_runtime_retention() {
    let run_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    // We use a short GC interval so the test doesn't need to wait for the 60s default.
    let test_runtime = test_runtime::TestRuntime::spawn("info", None, None, "100ms").await;
    let guard = test_runtime
        .spawned
        .telemetry_guard()
        .expect("telemetry guard must be present");

    // With no retention configured (the default) telemetry is not persisted at
    // all — the db is opt-in via a retention time.
    let off_msg = format!("OFF-BY-DEFAULT-{run_id}");
    tracing::info!("{off_msg}");
    // Metrics export bypasses the batched log/trace path, so gate coverage
    // needs its own signal: record a counter so the flush below has an
    // observation to export.
    let meter = opentelemetry::global::meter("retention-test");
    let counter = meter.u64_counter("retention_test_count").build();
    counter.add(1, &[]);
    guard.force_flush().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let bodies: Vec<String> = test_runtime
        .query_table::<LogRecord>(swarm_telemetry::db::TABLE_LOGS)
        .await
        .iter()
        .map(log_body)
        .collect();
    assert!(
        !bodies.iter().any(|b| b == &off_msg),
        "a log emitted without a db retention must not be persisted"
    );
    let metrics = test_runtime
        .query_table::<Metric>(swarm_telemetry::db::TABLE_METRICS)
        .await;
    assert!(
        !metrics.iter().any(|m| m.name == "retention_test_count"),
        "metrics must not be persisted without a db retention"
    );

    // Setting a retention at runtime turns persistence on.
    test_runtime
        .spawned
        .session()
        .put(swarm_telemetry::TOPIC_DB_RETENTION, "200ms")
        .await
        .expect("failed to publish db retention");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let on_msg = format!("WITH-RETENTION-{run_id}");
    tracing::info!("{on_msg}");
    guard.force_flush().unwrap();

    let bodies = test_runtime
        .await_data::<LogRecord, _, _, _>(
            swarm_telemetry::db::TABLE_LOGS,
            Duration::from_secs(2),
            log_body,
            |l| l == on_msg.as_str(),
        )
        .await;
    assert!(
        bodies.contains(&on_msg),
        "a log emitted with a db retention must be persisted"
    );

    // Wait for the 200ms retention to expire and the 100ms GC interval to fire.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let bodies: Vec<String> = test_runtime
        .query_table::<LogRecord>(swarm_telemetry::db::TABLE_LOGS)
        .await
        .iter()
        .map(log_body)
        .collect();
    assert!(
        !bodies.iter().any(|b| b == &on_msg),
        "a persisted log must be removed by GC after its retention expired"
    );

    // "null" turns persistence back off — the live kill switch.
    test_runtime
        .spawned
        .session()
        .put(swarm_telemetry::TOPIC_DB_RETENTION, "null")
        .await
        .expect("failed to publish db retention");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let off_again_msg = format!("OFF-AGAIN-{run_id}");
    tracing::info!("{off_again_msg}");
    guard.force_flush().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let bodies: Vec<String> = test_runtime
        .query_table::<LogRecord>(swarm_telemetry::db::TABLE_LOGS)
        .await
        .iter()
        .map(log_body)
        .collect();
    assert!(
        !bodies.iter().any(|b| b == &off_again_msg),
        "a log emitted after retention was cleared must not be persisted"
    );

    test_runtime.shutdown().await;
}
