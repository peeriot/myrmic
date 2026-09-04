use std::{collections::HashMap, time::Duration};

use cell_protocol::Sri;
use sorg_tests::{build_cell, swarm_config};
use swarm_telemetry::db::opentelemetry_proto::tonic::common::v1::any_value::Value;
use swarm_telemetry::db::opentelemetry_proto::tonic::metrics::v1::metric::Data;
use swarm_telemetry::db::opentelemetry_proto::tonic::metrics::v1::{Metric, number_data_point};
use swarm_telemetry::db::opentelemetry_proto::tonic::trace::v1::Span;

use crate::integration::spawn_test_app_with_swarm;

const INTERMEDIATE_SRI: &str = "trace_example_intermediate";
const SINK_SRI: &str = "trace_example_sink";

fn attr_str<'a>(span: &'a Span, key: &str) -> Option<&'a str> {
    use swarm_telemetry::db::opentelemetry_proto::tonic::common::v1::any_value::Value;
    span.attributes.iter().find(|a| a.key == key).and_then(|a| {
        a.value.as_ref()?.value.as_ref().and_then(|v| match v {
            Value::StringValue(s) => Some(s.as_str()),
            _ => None,
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "manipulates process-global OpenTelemetry state (init + guard.shutdown); \
            passes standalone (`cargo test ... cell_telemetry -- --ignored`) but the \
            telemetry guard is unavailable, and it poisons later tests, in a \
            shared-process suite run"]
#[expect(clippy::too_many_lines, reason = "it's a test, so it's fine")]
pub async fn cell_telemetry() {
    let swarm = swarm_config!("tracing.jsonnet");
    build_cell("../../tests/fixtures/trace_example_intermediate", &swarm).await;
    build_cell("../../tests/fixtures/trace_example_sink", &swarm).await;

    // Arrange - spawn the test app and load both test cells
    let test_app = spawn_test_app_with_swarm(swarm).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let guard = test_app
        .swarm_handle
        .telemetry_guard()
        .expect("telemetry guard must be present");

    test_app
        .deploy_wasm_cell(
            "trace_example_intermediate.wasm".to_owned(),
            INTERMEDIATE_SRI.to_owned(),
        )
        .await;
    test_app
        .deploy_wasm_cell("trace_example_sink.wasm".to_owned(), SINK_SRI.to_owned())
        .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Act - trigger the intermediate via a fire-and-forget command; the sleep
    // below gives its (and the sink's) span time to finish before we read them.
    let cmd_name = "intermediate";
    let rec_sri = INTERMEDIATE_SRI;
    test_app.command_send(rec_sri, cmd_name, None).await;

    // wait a tiny bit so that the cell's span can finish
    tokio::time::sleep(Duration::from_millis(500)).await;
    guard.shutdown().unwrap();

    let db_client = ::db_client::v1::Client::new(test_app.session());
    let result = db_client
        .read_tx_in(swarm_telemetry::db::scope(), async move |client, tx| {
            client
                .send(db_client::v1::models::tb_list::Request {
                    id: tx,
                    op: db_client::v1::models::tb_list::Op {
                        scope: swarm_telemetry::db::scope(),
                        table: swarm_telemetry::db::TABLE_TRACES.into(),
                        cursor: None,
                        limit: None,
                        order: None,
                    },
                })
                .await
        })
        .await
        .unwrap()
        .map_err(|err| err.message.clone())
        .unwrap();

    let spans = result
        .entities
        .iter()
        .rev()
        .map(|(id, data)| {
            let span_id = opentelemetry::SpanId::from_bytes(id[8..].try_into().unwrap());
            let entry =
                serde_json::from_slice::<swarm_telemetry::db::ScopedEntry<Span>>(data).unwrap();
            (span_id, entry.data)
        })
        .collect::<HashMap<_, _>>();

    // Cells are identified in telemetry by their SRI (a UUID), not their name.
    let sink_id = Sri::from_target(SINK_SRI).unwrap().to_string();
    let intermediate_id = Sri::from_target(INTERMEDIATE_SRI).unwrap().to_string();

    // find the one child span that has parent set
    let child_span = spans
        .values()
        .find(|span| {
            !span.parent_span_id.iter().all(|b| *b == 0)
                && span.name == "cell_task::message_handler"
        })
        .unwrap();
    let parent_span_id =
        opentelemetry::SpanId::from_bytes(child_span.parent_span_id.clone().try_into().unwrap());
    assert_eq!(child_span.name.as_str(), "cell_task::message_handler");
    assert_eq!(attr_str(child_span, "module_id"), Some(sink_id.as_str()));
    assert_ne!(
        child_span.start_time_unix_nano,
        child_span.end_time_unix_nano
    );

    // now find the related parent span
    let parent_span = spans.get(&parent_span_id).unwrap();
    assert_eq!(parent_span.name.as_str(), "cell_task::message_handler");
    assert_eq!(
        attr_str(parent_span, "module_id"),
        Some(intermediate_id.as_str())
    );
    assert_ne!(
        parent_span.start_time_unix_nano,
        parent_span.end_time_unix_nano
    );

    // PARKED(new-model): guest log-body assertions are disabled — the runtime
    // does not export guest logs to the telemetry DB (`logging.rs::log` only
    // `println!`s; the tracing/OTEL emission is commented out). The span (above)
    // and metric (below) assertions cover the cell-to-cell fire-and-forget path.

    // check metrics stored in the DB
    let metrics_result = db_client
        .read_tx_in(swarm_telemetry::db::scope(), async move |client, tx| {
            client
                .send(db_client::v1::models::tb_list::Request {
                    id: tx,
                    op: db_client::v1::models::tb_list::Op {
                        scope: swarm_telemetry::db::scope(),
                        table: swarm_telemetry::db::TABLE_METRICS.into(),
                        cursor: None,
                        limit: None,
                        order: None,
                    },
                })
                .await
        })
        .await
        .unwrap()
        .map_err(|err| err.message.clone())
        .unwrap();

    let metrics: Vec<Metric> = metrics_result
        .entities
        .iter()
        .filter_map(|(_, data)| {
            let entry =
                serde_json::from_slice::<swarm_telemetry::db::ScopedEntry<Metric>>(data).ok()?;
            Some(entry.data)
        })
        .collect();

    let mut sink_processed = 0;
    let mut intermediate_processed = 0;

    for metric in &metrics {
        if metric.name.as_str() == "cell_commands_processed"
            && let Data::Sum(sum) = metric.data.as_ref().unwrap()
        {
            for dp in &sum.data_points {
                let sri = dp
                    .attributes
                    .iter()
                    .find(|att| att.key.as_str() == "sri")
                    .unwrap();

                let number_data_point::Value::AsInt(value) = dp.value.unwrap() else {
                    panic!("cell_commands_processed must be an integer");
                };

                if let Value::StringValue(sri) = sri.value.as_ref().unwrap().value.as_ref().unwrap()
                {
                    let sri = sri.as_str();
                    if sri == sink_id.as_str() {
                        sink_processed = value;
                    } else if sri == intermediate_id.as_str() {
                        intermediate_processed = value;
                    } else {
                        panic!("sri={sri}");
                    }
                }
            }
        }
    }

    assert_eq!(1, sink_processed);
    assert_eq!(1, intermediate_processed);
}
