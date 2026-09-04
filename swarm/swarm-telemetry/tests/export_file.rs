#![cfg(feature = "export-file")]

//! The file exporter needs no zenoh session or db plugin — these tests drive
//! the `OTel` providers against a [`FileExporter`] directly and read the files
//! back the way `test-framework`'s fetcher does (JSON-lines of `ScopedEntry`).

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::trace::{Tracer, TracerProvider as _};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider};
use swarm_telemetry::export::ScopedEntry;
use swarm_telemetry::file::opentelemetry_proto::tonic::metrics::v1::Metric;
use swarm_telemetry::file::opentelemetry_proto::tonic::trace::v1::Span;
use swarm_telemetry::file::{FILE_METRICS_LATEST, FILE_TRACES, FileExporter};

const TEST_SPAN_NAME: &str = "swarm_telemetry_export_file_test_span";
const TEST_COUNTER_NAME: &str = "swarm_telemetry_export_file_test_counter";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exports_spans_as_scoped_entry_json_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exporter = FileExporter::new(dir.path().to_path_buf()).expect("file exporter");

    let provider = SdkTracerProvider::builder()
        .with_span_processor(BatchSpanProcessor::builder(exporter).build())
        .build();

    let tracer = provider.tracer("file-export-test");
    tracer.in_span(TEST_SPAN_NAME, |_cx| {});
    tracer.in_span(TEST_SPAN_NAME, |_cx| {});
    provider.force_flush().expect("force flush");

    let traces = std::fs::read_to_string(dir.path().join(FILE_TRACES)).expect("traces file exists");
    let spans: Vec<ScopedEntry<Span>> = traces
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line decodes"))
        .collect();

    let exported: Vec<_> = spans
        .iter()
        .filter(|entry| entry.data.name == TEST_SPAN_NAME)
        .collect();
    assert_eq!(exported.len(), 2, "both spans exported, appended");
    for entry in exported {
        assert_eq!(entry.scope_name.as_deref(), Some("file-export-test"));
        assert!(!entry.data.trace_id.is_empty());
        assert!(entry.data.end_time_unix_nano >= entry.data.start_time_unix_nano);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_latest_is_rewritten_not_appended() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exporter = FileExporter::new(dir.path().to_path_buf()).expect("file exporter");

    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter).build())
        .build();

    let counter = provider
        .meter("file-export-test")
        .u64_counter(TEST_COUNTER_NAME)
        .build();

    counter.add(3, &[]);
    provider.force_flush().expect("force flush");
    counter.add(2, &[]);
    provider.force_flush().expect("force flush");

    let metrics = std::fs::read_to_string(dir.path().join(FILE_METRICS_LATEST))
        .expect("metrics-latest file exists");
    let entries: Vec<ScopedEntry<Metric>> = metrics
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line decodes"))
        .filter(|entry: &ScopedEntry<Metric>| entry.data.name == TEST_COUNTER_NAME)
        .collect();

    // The file is the *latest* snapshot: one line per metric however many
    // exports ran, carrying the cumulative value.
    assert_eq!(entries.len(), 1, "rewritten per export, not appended");
    assert_eq!(entries[0].scope_name.as_deref(), Some("file-export-test"));
}
