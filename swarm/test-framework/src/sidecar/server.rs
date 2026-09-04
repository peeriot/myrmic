//! axum server exposing zenoh, sorg, introspection and test-control operations
//! over HTTP, so tests can observe and inject traffic from a node's perspective
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use introspection_client::v1::Client as IntrospectionClient;
use serde::Deserialize;
use serde_json::json;
use sorg_client::Client as SorgClient;
use test_control_client::Client as TestControlClient;
use test_control_common::Reply;
use tokio::sync::Mutex;
use zenoh::Session;

#[derive(Deserialize)]
struct ExecRuntimesRequest {
    endpoint: String,
    mode: Option<String>,
}

#[derive(Deserialize)]
struct StartQueryableRequest {
    endpoint: String,
    mode: Option<String>,
    key_expr: String,
    payload: String,
}

#[derive(Deserialize)]
struct ZenohGetRequest {
    endpoint: String,
    mode: Option<String>,
    key_expr: String,
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct StartSubscriberRequest {
    endpoint: String,
    mode: Option<String>,
    key_expr: String,
}

#[derive(Deserialize)]
struct PublishRequest {
    endpoint: String,
    mode: Option<String>,
    key_expr: String,
    count: usize,
}

#[derive(Deserialize)]
struct TestControlCreateSubscriberRequest {
    endpoint: String,
    mode: Option<String>,
    zid: String,
    key_expr: String,
    max_samples: Option<u32>,
    stream_key: Option<String>,
}

#[derive(Deserialize)]
struct TestControlCreatePublisherRequest {
    endpoint: String,
    mode: Option<String>,
    zid: String,
    key_expr: String,
    payload: String,
    count: Option<u32>,
    delay_ms: Option<u64>,
}

#[derive(Deserialize)]
struct TestControlStatsRequest {
    endpoint: String,
    mode: Option<String>,
    zid: String,
    key_expr: String,
}

struct ActiveQueryable {
    _session: Session,
    _task: tokio::task::JoinHandle<()>,
}

struct ActiveSubscriber {
    _session: Session,
    _subscriber: zenoh::pubsub::Subscriber<()>,
    counter: Arc<AtomicUsize>,
}

#[derive(Default)]
struct AppState {
    queryables: Mutex<Vec<ActiveQueryable>>,
    subscribers: Mutex<Vec<ActiveSubscriber>>,
}

/// Run the sidecar server, listening on `SIDECAR_LISTEN_ADDR` (default `0.0.0.0:8080`).
pub async fn run() {
    let listen_addr = std::env::var("SIDECAR_LISTEN_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
        .parse::<SocketAddr>()
        .expect("invalid SIDECAR_LISTEN_ADDR");

    let state = Arc::new(AppState::default());

    let app = Router::new()
        .route("/exec-runtimes", get(list_exec_runtimes))
        .route("/swarm-status", get(get_swarm_status))
        .route("/own-status", get(get_own_status))
        .route("/zenoh/start-queryable", post(start_queryable))
        .route("/zenoh/get", get(zenoh_get_handler))
        .route("/zenoh/start-subscriber", post(start_subscriber))
        .route(
            "/zenoh/subscriber/{sub_id}/count",
            get(get_subscriber_count),
        )
        .route("/zenoh/publish", post(publish_messages))
        .route(
            "/test-control/subscriber",
            post(test_control_create_subscriber),
        )
        .route(
            "/test-control/publisher",
            post(test_control_create_publisher),
        )
        .route("/test-control/stats", get(test_control_stats))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(listen_addr)
        .await
        .expect("failed to bind sidecar listener");
    axum::serve(listener, app)
        .await
        .expect("sidecar server failed");
}

async fn list_exec_runtimes(Query(request): Query<ExecRuntimesRequest>) -> impl IntoResponse {
    match fetch_exec_runtimes(&request.endpoint, request.mode.as_deref()).await {
        Ok(runtimes) => (
            StatusCode::OK,
            Json(json!({
                "count": runtimes.len(),
                "runtimes": runtimes,
            })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": err,
            })),
        )
            .into_response(),
    }
}

async fn get_own_status(Query(request): Query<ExecRuntimesRequest>) -> impl IntoResponse {
    match fetch_own_status(&request.endpoint, request.mode.as_deref()).await {
        Ok(status) => (StatusCode::OK, Json(json!(status))).into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": err,
            })),
        )
            .into_response(),
    }
}

async fn get_swarm_status(Query(request): Query<ExecRuntimesRequest>) -> impl IntoResponse {
    match fetch_swarm_status(&request.endpoint, request.mode.as_deref()).await {
        Ok(statuses) => (StatusCode::OK, Json(json!(statuses))).into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": err,
            })),
        )
            .into_response(),
    }
}

async fn start_queryable(
    State(state): State<Arc<AppState>>,
    Query(request): Query<StartQueryableRequest>,
) -> impl IntoResponse {
    let result: Result<(), String> = async {
        let endpoint = resolve_endpoint(&request.endpoint).await?;
        let session = zenoh_session(&endpoint, request.mode.as_deref())
            .await
            .map_err(|err| format!("failed to open zenoh session: {err}"))?;

        let qbl = session
            .declare_queryable(&request.key_expr)
            .await
            .map_err(|err| format!("failed to declare queryable: {err}"))?;

        let payload = request.payload.clone();
        let key_expr = request.key_expr.clone();
        let task = tokio::spawn(async move {
            while let Ok(query) = qbl.recv_async().await {
                let _ = query.reply(key_expr.as_str(), payload.clone()).await;
            }
        });

        state.queryables.lock().await.push(ActiveQueryable {
            _session: session,
            _task: task,
        });

        Ok(())
    }
    .await;

    match result {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(err) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": err }))).into_response(),
    }
}

async fn start_subscriber(
    State(state): State<Arc<AppState>>,
    Query(request): Query<StartSubscriberRequest>,
) -> impl IntoResponse {
    let result: Result<usize, String> = async {
        let endpoint = resolve_endpoint(&request.endpoint).await?;
        let session = zenoh_session(&endpoint, request.mode.as_deref())
            .await
            .map_err(|err| format!("failed to open zenoh session: {err}"))?;

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let subscriber = session
            .declare_subscriber(&request.key_expr)
            .callback(move |_sample| {
                counter_clone.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .map_err(|err| format!("failed to declare subscriber: {err}"))?;

        let mut subscribers = state.subscribers.lock().await;
        let sub_id = subscribers.len();
        subscribers.push(ActiveSubscriber {
            _session: session,
            _subscriber: subscriber,
            counter,
        });

        Ok(sub_id)
    }
    .await;

    match result {
        Ok(sub_id) => (StatusCode::OK, Json(json!({ "sub_id": sub_id }))).into_response(),
        Err(err) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": err }))).into_response(),
    }
}

async fn get_subscriber_count(
    State(state): State<Arc<AppState>>,
    Path(sub_id): Path<usize>,
) -> impl IntoResponse {
    let subscribers = state.subscribers.lock().await;
    match subscribers.get(sub_id) {
        Some(sub) => {
            let count = sub.counter.load(Ordering::Relaxed);
            (StatusCode::OK, Json(json!({ "count": count }))).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("subscriber {sub_id} not found") })),
        )
            .into_response(),
    }
}

async fn publish_messages(Query(request): Query<PublishRequest>) -> impl IntoResponse {
    let result: Result<(), String> = async {
        let endpoint = resolve_endpoint(&request.endpoint).await?;
        let session = zenoh_session(&endpoint, request.mode.as_deref())
            .await
            .map_err(|err| format!("failed to open zenoh session: {err}"))?;

        let publisher = session
            .declare_publisher(&request.key_expr)
            .await
            .map_err(|err| format!("failed to declare publisher: {err}"))?;

        for _ in 0..request.count {
            publisher
                .put("hello")
                .await
                .map_err(|err| format!("failed to publish: {err}"))?;
        }

        Ok(())
    }
    .await;

    match result {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Err(err) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": err }))).into_response(),
    }
}

async fn zenoh_get_handler(Query(request): Query<ZenohGetRequest>) -> impl IntoResponse {
    let result: Result<usize, String> = async {
        let endpoint = resolve_endpoint(&request.endpoint).await?;
        let session = zenoh_session(&endpoint, request.mode.as_deref())
            .await
            .map_err(|err| format!("failed to open zenoh session: {err}"))?;

        let timeout = Duration::from_millis(request.timeout_ms.unwrap_or(5000));

        let replies = session
            .get(&request.key_expr)
            .timeout(timeout)
            .await
            .map_err(|err| format!("zenoh get failed: {err}"))?;

        let mut count = 0usize;
        while let Ok(reply) = replies.recv_async().await {
            if reply.result().is_ok() {
                count += 1;
            }
        }

        Ok(count)
    }
    .await;

    match result {
        Ok(count) => (StatusCode::OK, Json(json!({ "replies": count }))).into_response(),
        Err(err) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": err }))).into_response(),
    }
}

async fn test_control_create_subscriber(
    Query(request): Query<TestControlCreateSubscriberRequest>,
) -> impl IntoResponse {
    let result: Result<Reply, String> = async {
        let client = test_control_client(&request.endpoint, request.mode.as_deref()).await?;
        client
            .create_subscriber(
                request.zid,
                request.key_expr,
                request.max_samples,
                request.stream_key,
            )
            .await
            .map_err(|err| format!("failed to create test-control subscriber: {err}"))
    }
    .await;

    match result {
        Ok(Reply::SubscriberCreated {
            ok,
            sub_id,
            key_expr,
        }) => (
            StatusCode::OK,
            Json(json!({ "ok": ok, "sub_id": sub_id, "key_expr": key_expr })),
        )
            .into_response(),
        Ok(reply) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("unexpected test-control reply: {reply:?}") })),
        )
            .into_response(),
        Err(err) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": err }))).into_response(),
    }
}

async fn test_control_create_publisher(
    Query(request): Query<TestControlCreatePublisherRequest>,
) -> impl IntoResponse {
    let result: Result<Reply, String> = async {
        let client = test_control_client(&request.endpoint, request.mode.as_deref()).await?;
        client
            .create_publisher(
                request.zid,
                request.key_expr,
                request.payload,
                request.count,
                request.delay_ms.map(Duration::from_millis),
            )
            .await
            .map_err(|err| format!("failed to create test-control publisher: {err}"))
    }
    .await;

    match result {
        Ok(Reply::PublisherCreated {
            ok,
            pub_id,
            key_expr,
        }) => (
            StatusCode::OK,
            Json(json!({ "ok": ok, "pub_id": pub_id, "key_expr": key_expr })),
        )
            .into_response(),
        Ok(reply) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("unexpected test-control reply: {reply:?}") })),
        )
            .into_response(),
        Err(err) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": err }))).into_response(),
    }
}

async fn test_control_stats(Query(request): Query<TestControlStatsRequest>) -> impl IntoResponse {
    let result: Result<Reply, String> = async {
        let client = test_control_client(&request.endpoint, request.mode.as_deref()).await?;
        client
            .stats(request.zid, request.key_expr)
            .await
            .map_err(|err| format!("failed to get test-control stats: {err}"))
    }
    .await;

    match result {
        Ok(Reply::Stats {
            ok,
            key_expr,
            sent,
            received,
            gets,
            queries,
        }) => (
            StatusCode::OK,
            Json(json!({
                "ok": ok,
                "key_expr": key_expr,
                "sent": sent,
                "received": received,
                "gets": gets,
                "queries": queries,
            })),
        )
            .into_response(),
        Ok(reply) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("unexpected test-control reply: {reply:?}") })),
        )
            .into_response(),
        Err(err) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": err }))).into_response(),
    }
}

async fn fetch_exec_runtimes(
    endpoint: &str,
    mode: Option<&str>,
) -> Result<Vec<cell_protocol::ExecRuntimeInfo>, String> {
    let endpoint = resolve_endpoint(endpoint).await?;
    let session = zenoh_session(&endpoint, mode)
        .await
        .map_err(|err| format!("failed to open zenoh session: {err}"))?;
    let client = SorgClient::new(session);
    client
        .list_exec_runtimes()
        .await
        .map_err(|err| format!("failed to list exec runtimes: {err}"))
}

async fn fetch_own_status(
    endpoint: &str,
    mode: Option<&str>,
) -> Result<introspection_client::v1::NodeStatus, String> {
    let endpoint = resolve_endpoint(endpoint).await?;
    let session = zenoh_session(&endpoint, mode)
        .await
        .map_err(|err| format!("failed to open zenoh session: {err}"))?;
    let client = IntrospectionClient::new(session).await;
    client
        .own_status()
        .await
        .map_err(|err| format!("failed to fetch own status: {err}"))
}

async fn fetch_swarm_status(
    endpoint: &str,
    mode: Option<&str>,
) -> Result<Vec<introspection_client::v1::NodeStatus>, String> {
    let endpoint = resolve_endpoint(endpoint).await?;
    let session = zenoh_session(&endpoint, mode)
        .await
        .map_err(|err| format!("failed to open zenoh session: {err}"))?;
    let client = IntrospectionClient::new(session).await;
    client
        .swarm_status()
        .await
        .map_err(|err| format!("failed to fetch swarm status: {err}"))
}

async fn test_control_client(
    endpoint: &str,
    mode: Option<&str>,
) -> Result<TestControlClient, String> {
    let endpoint = resolve_endpoint(endpoint).await?;
    let session = zenoh_session(&endpoint, mode)
        .await
        .map_err(|err| format!("failed to open zenoh session: {err}"))?;
    Ok(TestControlClient::new(session))
}

async fn resolve_endpoint(endpoint: &str) -> Result<String, String> {
    let Some(rest) = endpoint.strip_prefix("tcp/") else {
        return Ok(endpoint.to_owned());
    };
    let Some((host, port)) = rest.rsplit_once(':') else {
        return Ok(endpoint.to_owned());
    };

    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(endpoint.to_owned());
    }

    let port = port
        .parse::<u16>()
        .map_err(|err| format!("invalid zenoh port in endpoint `{endpoint}`: {err}"))?;
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|err| format!("failed to resolve `{host}`: {err}"))?;
    let resolved = addrs
        .next()
        .ok_or_else(|| format!("no addresses found for `{host}`"))?;

    Ok(format!("tcp/{}:{}", resolved.ip(), resolved.port()))
}

async fn zenoh_session(endpoint: &str, mode: Option<&str>) -> Result<Session, zenoh::Error> {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("mode", &format!(r#""{}""#, mode.unwrap_or("client")))
        .expect("zenoh mode");
    config
        .insert_json5("connect/endpoints", &format!(r#"["{endpoint}"]"#))
        .expect("zenoh endpoints");
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .expect("zenoh multicast");
    config
        .insert_json5("open/return_conditions/connect_scouted", "true")
        .expect("zenoh open connect_scouted");
    config
        .insert_json5("open/return_conditions/declares", "true")
        .expect("zenoh open declares");

    // Keep per-request session setup bounded so the sidecar does not hang forever on failures.
    tokio::time::timeout(Duration::from_secs(5), zenoh::open(config))
        .await
        .map_err(|_| {
            zenoh::Error::from(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out opening zenoh session",
            ))
        })?
}
