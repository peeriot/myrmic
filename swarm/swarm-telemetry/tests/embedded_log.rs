#![cfg(feature = "export-db")]

mod test_runtime;
mod test_runtime_db;

use std::time::Duration;

use swarm_telemetry::db::opentelemetry_proto::tonic::common::v1::any_value::Value;
use swarm_telemetry::db::opentelemetry_proto::tonic::logs::v1::LogRecord;
use swarm_telemetry_embedded::{Level, TOPIC_LOGS, TelemetryRecord};

fn make_record(level: Level, target: &str, message: &str) -> TelemetryRecord {
    TelemetryRecord {
        level,
        target: heapless::String::try_from(target).expect("target too long"),
        message: heapless::String::try_from(message).expect("message too long"),
    }
}

async fn publish(runtime: &test_runtime::TestRuntime, record: &TelemetryRecord) {
    let bytes = postcard::to_allocvec(record).expect("serialization failed");
    runtime
        .spawned
        .session()
        .put(TOPIC_LOGS, bytes)
        .await
        .expect("put failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn embedded_log_plugin() {
    let test_runtime = test_runtime::TestRuntime::spawn("info", None, Some("1h"), "60s").await;

    let guard = test_runtime
        .spawned
        .telemetry_guard()
        .expect("telemetry guard must be present");

    // invalid payload must be silently skipped (worker stays alive)
    test_runtime
        .spawned
        .session()
        .put(TOPIC_LOGS, b"not valid postcard".as_slice())
        .await
        .expect("put failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // valid record after the bad one — confirms the worker is still running
    let record = make_record(Level::Info, "my::module", "hello from embedded");
    publish(&test_runtime, &record).await;

    guard.force_flush().unwrap();

    let logs = test_runtime
        .await_data::<LogRecord, _, _, _>(
            swarm_telemetry::db::TABLE_LOGS,
            Duration::from_secs(2),
            std::clone::Clone::clone,
            |log| {
                let has_target = log.attributes.iter().any(|a| {
                    a.key == "embedded_target"
                        && matches!(
                            a.value.as_ref().and_then(|v| v.value.as_ref()),
                            Some(Value::StringValue(s)) if s == "my::module"
                        )
                });
                let has_body = matches!(
                    log.body.as_ref().and_then(|b| b.value.as_ref()),
                    Some(Value::StringValue(s)) if s == "hello from embedded"
                );
                has_target && has_body
            },
        )
        .await;

    assert!(
        !logs.is_empty(),
        "expected embedded log record in DB with embedded_target=my::module"
    );

    test_runtime.shutdown().await;
}
