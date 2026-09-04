#![cfg(feature = "export-otlp")]

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
    trace_service_server::{TraceService, TraceServiceServer},
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::TcpListenerStream;

mod test_runtime;

const TEST_SPAN_NAME: &str = "swarm_telemetry_export_otlp_test_span";

/// Forwards every `Export` call it receives onto a channel so a test can
/// inspect what the SDK actually sent. `mpsc::Sender::send` only needs `&self`,
/// so no interior-mutability wrapper is required here.
struct ExportRelay {
    received: mpsc::Sender<ExportTraceServiceRequest>,
}

#[tonic::async_trait]
impl TraceService for ExportRelay {
    async fn export(
        &self,
        request: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
        self.received
            .send(request.into_inner())
            .await
            .map_err(|err| {
                tonic::Status::unavailable(format!("test dropped the receiving end: {err}"))
            })?;

        Ok(tonic::Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

/// Starts a throwaway OTLP/gRPC collector on an OS-assigned loopback port and
/// hands back its address plus the receiving half of the channel every
/// exported request lands on. Returns `None` only when this sandbox forbids
/// binding a socket at all; any other bind failure is a real test failure.
async fn setup_trace_collector() -> Option<(SocketAddr, mpsc::Receiver<ExportTraceServiceRequest>)>
{
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return None,
        Err(err) => panic!("failed to bind the mock OTLP collector: {err}"),
    };
    let addr = listener
        .local_addr()
        .expect("bound socket carries a local address");

    let (received_tx, received_rx) = mpsc::channel(1);
    let relay = TraceServiceServer::new(ExportRelay {
        received: received_tx,
    });

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(relay)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("mock OTLP collector exited unexpectedly");
    });

    Some((addr, received_rx))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn exports_sample_span_to_otlp_collector() {
    let Some((collector_addr, mut requests)) = setup_trace_collector().await else {
        eprintln!("skipping OTLP collector test because local TCP bind is not permitted");
        return;
    };

    let runtime = test_runtime::TestRuntime::spawn("info", Some(collector_addr), None, "60s").await;

    let guard = runtime
        .spawned
        .telemetry_guard()
        .expect("telemetry guard must be present");

    {
        let span = tracing::info_span!(TEST_SPAN_NAME, test_attr = "expected");
        let _entered = span.enter();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    guard.shutdown();

    let request = tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await
        .expect("collector should receive request before timeout")
        .expect("collector channel should yield one request");

    let span = request
        .resource_spans
        .iter()
        .flat_map(|resource| resource.scope_spans.iter())
        .flat_map(|scope| scope.spans.iter())
        .find(|span| span.name == TEST_SPAN_NAME)
        .expect("expected exported span");

    assert_eq!(span.name, TEST_SPAN_NAME);

    let attributes = span
        .attributes
        .iter()
        .map(|attr| (attr.key.as_str(), attr.value.as_ref()))
        .collect::<HashMap<_, _>>();
    let test_attr = attributes
        .get("test_attr")
        .and_then(|value| value.and_then(|value| value.value.as_ref()))
        .map(|value| format!("{value:?}"));

    assert_eq!(test_attr.as_deref(), Some("StringValue(\"expected\")"));

    let service_name = request
        .resource_spans
        .iter()
        .flat_map(|resource| resource.resource.iter())
        .flat_map(|resource| resource.attributes.iter())
        .find(|attr| attr.key == "service.name")
        .and_then(|attr| attr.value.as_ref())
        .and_then(|value| value.value.as_ref())
        .map(|value| format!("{value:?}"));

    assert_eq!(service_name.as_deref(), Some("StringValue(\"swarm\")"));
}
