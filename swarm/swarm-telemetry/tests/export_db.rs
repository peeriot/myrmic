#![cfg(feature = "export-db")]

mod test_runtime;
mod test_runtime_db;

use std::time::Duration;

use swarm_telemetry::db::opentelemetry_proto::tonic::{
    common::v1::any_value::Value, trace::v1::Span,
};

const TEST_SPAN_NAME: &str = "swarm_telemetry_export_db_test_span";

fn attr_str<'a>(span: &'a Span, key: &str) -> Option<&'a str> {
    span.attributes.iter().find(|a| a.key == key).and_then(|a| {
        a.value.as_ref()?.value.as_ref().and_then(|v| match v {
            Value::StringValue(s) => Some(s.as_str()),
            _ => None,
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn exports_sample_span_to_db() {
    let test_runtime = test_runtime::TestRuntime::spawn("info", None, Some("1h"), "60s").await;

    let guard = test_runtime
        .spawned
        .telemetry_guard()
        .expect("telemetry guard must be present");

    let run_id = format!(
        "run-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    {
        let span = tracing::info_span!(
            TEST_SPAN_NAME,
            test_run_id = run_id.as_str(),
            test_attr = "expected"
        );
        let _entered = span.enter();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    guard.force_flush().unwrap();

    let spans = test_runtime
        .await_data::<Span, _, _, _>(
            swarm_telemetry::db::TABLE_TRACES,
            Duration::from_secs(1),
            std::clone::Clone::clone,
            |s| s.name == TEST_SPAN_NAME && attr_str(s, "test_run_id") == Some(run_id.as_str()),
        )
        .await;

    let span = spans
        .into_iter()
        .find(|s| s.name == TEST_SPAN_NAME && attr_str(s, "test_run_id") == Some(run_id.as_str()))
        .expect("expected exported span in DB");

    assert_eq!(span.name, TEST_SPAN_NAME);
    assert_eq!(attr_str(&span, "test_run_id"), Some(run_id.as_str()));
    assert_eq!(attr_str(&span, "test_attr"), Some("expected"));
    assert!(!span.trace_id.is_empty());
    assert!(span.end_time_unix_nano >= span.start_time_unix_nano);

    test_runtime.shutdown().await;
}
