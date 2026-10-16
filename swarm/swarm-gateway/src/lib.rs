//! The myrmic socket gateway.
//!
//! A native process that joins the swarm as a peer and exposes a socket
//! (HTTP / `WebSocket`) entrypoint into the network.
//!
//! Cells declare the URL prefix they serve, and the command requests under it
//! go to, in the `gateway-config` datalayer registry (see
//! [`sorg_common::gateway_config`]). Every gateway discovers and watches that
//! registry, so all gateways in the network serve the same routes.
//!
//! Each request is sent to its route's cell as an [`HttpRequest`]; the cell
//! answers with a response, a blob to serve, or a `WebSocket` upgrade. The
//! gateway itself only guards routes (OIDC) and reads the blob store.
//!
//! This crate provides the server ([`run`]); the `myrmic gateway` CLI command
//! is the primary way to launch it.

use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context as _;
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header, request};
use axum::response::{IntoResponse, Redirect, Response};
use myrmic_common::cells::Command;
use myrmic_common::gateway::{self as wire, HttpReply, HttpRequest, HttpResponse};
use serde::Deserialize;
use sorg_common::gateway_config::GatewayRoute;
use tokio::sync::{mpsc, watch};
use tower_sessions::cookie::time;
use uuid::Uuid;
use zenoh::Session;

use crate::relay::{Relay, Reply};

mod assets;
pub mod oidc;
mod relay;
pub mod routes;
mod ws;

const DEFAULT_SESSION_INACTIVITY_TIMER_SECS: i64 = 120;
const MINIMUM_SESSION_INACTIVITY_TIMER_SECS: i64 = 2;

/// The port the gateway binds to when none is configured.
pub const DEFAULT_PORT: u16 = 8080;

/// Maximum accepted request body size (1 `MiB`).
const MAX_HTTP_BODY: usize = 1 << 20;

/// How long a cell has to answer a request before the client gets a 504.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// How often registering the gateway's SRI is retried while the db is
/// unreachable.
const REGISTER_RETRY: Duration = Duration::from_secs(2);

/// How long shutdown waits for open sockets to tell their cells they closed.
const SOCKET_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The cookie carrying the gateway's own session (the OIDC login).
const SESSION_COOKIE: &str = "myrmic_gateway_session";

/// Operator-provided configuration for a gateway process.
///
/// This is the configuration an administrator hands to `myrmic gateway`. Which
/// routes exist is still discovered from the network at runtime; what an
/// operator controls here is the socket, the session cookies, and — through
/// [`RoutingConfig`] — which of those routes this gateway will serve and what
/// guards them.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Config {
    /// TCP port to bind. Defaults to [`DEFAULT_PORT`].
    #[serde(default)]
    pub port: Option<u16>,

    /// Whether the gateway is reached over HTTPS, which marks its session
    /// cookies `Secure`. Set it when a TLS terminator sits in front.
    #[serde(default)]
    pub over_https: bool,

    /// How long a session may sit idle before it expires. Defaults to 120s, floored at 2s.
    #[serde(default)]
    pub session_inactivity_timer_secs: Option<i64>,

    /// Which discovered routes to serve, and the OIDC provider guarding each —
    /// either inline, or a path to a JSON file holding the same. `None` serves
    /// every route that is discovered, unguarded.
    #[serde(default, flatten)]
    pub routing: Option<Either<PathBuf, RoutingConfig>>,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(untagged)]
pub enum Either<L, R> {
    Left(L),
    Right(R),
}

/// Which of the network's routes this gateway serves, keyed by mount path.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub base_url: Option<String>,

    /// The mounts this gateway will serve. `None` accepts every discovered
    /// route; otherwise a route is served only if its mount appears here.
    #[serde(default)]
    pub routes: Option<HashMap<String, RouteConfig>>,
}

/// How this gateway treats one mount.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RouteConfig {
    /// The only cell allowed to own this mount. `None` accepts whichever cell
    /// claimed it first.
    #[serde(default)]
    pub srn: Option<String>,

    /// The provider guarding this mount. `None` leaves it public.
    #[serde(default)]
    pub oidc: Option<OidcConfig>,
}

/// Per-route OIDC configuration
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OidcConfig {
    pub application_base_url: Option<String>,

    pub issuer: String,

    pub client_id: String,

    #[serde(default)]
    pub client_secret: Option<String>,

    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    session: Session,
    routes: routes::Routes,
    relay: Relay,

    routing: Option<RoutingView>,

    http_client: reqwest::Client,

    /// Flips to `true` once the gateway is shutting down.
    shutdown: watch::Receiver<bool>,
    /// Held by every open socket, so shutdown can wait for them to close.
    sockets_alive: mpsc::Sender<()>,
}

pub type RoutingView = Arc<RwLock<RoutingConfig>>;

/// Runs the gateway server until `shutdown` resolves.
///
/// `session` is a live swarm (zenoh) session used to reach the datalayer and
/// cells; the gateway keeps it alive for the whole server lifetime.
///
/// `ready` is notified once the listening socket is bound — i.e. once the
/// gateway is actually accepting connections. The swarm plugin host waits on
/// this to know startup succeeded, so it is signalled only *after* the bind
/// (a bind failure returns an error and leaves `ready` un-notified).
pub async fn run<F>(
    config: Config,
    session: Session,
    ready: Arc<tokio::sync::Notify>,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let Config {
        port,
        over_https,
        session_inactivity_timer_secs,
        routing,
    } = config;

    let session_inactivity_timer_secs = session_inactivity_timer_secs
        .unwrap_or(DEFAULT_SESSION_INACTIVITY_TIMER_SECS)
        .max(MINIMUM_SESSION_INACTIVITY_TIMER_SECS);

    let routing = match routing {
        Some(Either::Left(path)) => {
            let value = std::fs::read_to_string(&path)
                .with_context(|| format!("unable to read routing config: {}", path.display()))?;
            let config = serde_json::from_str::<RoutingConfig>(&value)
                .with_context(|| format!("unable to parse routing config: {}", path.display()))?;

            // @TODO jezza - 12 Aug 2026: Spawn a tokio task that monitors the file and refreshes the routing config.

            Some(Arc::new(RwLock::new(config)))
        }
        Some(Either::Right(config)) => Some(Arc::new(RwLock::new(config))),
        None => None,
    };

    let port = port.unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::from((Ipv6Addr::UNSPECIFIED, port));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("unable to bind gateway to {addr}"))?;

    match listener.local_addr() {
        Ok(bound) => tracing::info!("gateway listening on {bound}"),
        Err(err) => tracing::info!("gateway listening on {addr} (local addr unavailable: {err})"),
    }

    // The socket is bound and listening; the gateway is up.
    ready.notify_one();

    let (stopping, mut shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        let _ = stopping.send(true);
    });

    let Some((relay, relay_gen)) = register(&session, &mut shutdown_rx).await else {
        return Ok(());
    };
    let pump = relay.spawn_pump();

    let (routes, discovery) = routes::spawn_discovery(&session, routing.as_ref());

    // Redirects are the caller's business: the OIDC flow inspects the provider's
    // 3xx responses itself.
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("unable to build the gateway's http client")?;

    let (sockets_alive, mut sockets_done) = mpsc::channel(1);
    let state = AppState {
        session,
        routes,
        relay: relay.clone(),
        routing,
        http_client,
        shutdown: shutdown_rx.clone(),
        sockets_alive,
    };

    let sessions = tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default())
        .with_name(SESSION_COOKIE)
        .with_secure(over_https)
        .with_expiry(tower_sessions::Expiry::OnInactivity(
            time::Duration::seconds(session_inactivity_timer_secs),
        ));

    let app = axum::Router::new()
        .fallback(handler)
        .with_state(state)
        .layer(sessions)
        .layer(DefaultBodyLimit::max(4096));
    // .layer(CorsLayer::permissive())
    // .layer(TraceLayer::new_for_http());

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.wait_for(|&stopping| stopping).await;
        })
        .await
        .context("gateway server error");

    // The router, and every sender but the sockets', is gone with the server;
    // the channel closes once the last socket has told its cell.
    let _ = tokio::time::timeout(SOCKET_DRAIN_TIMEOUT, sockets_done.recv()).await;

    discovery.abort();
    pump.abort();
    relay.stop(relay_gen).await;

    result?;
    tracing::info!("gateway stopped");
    Ok(())
}

/// Registers the gateway's SRI, retrying while the db is unreachable. `None`
/// if the gateway was shut down first.
async fn register(
    session: &Session,
    shutdown: &mut watch::Receiver<bool>,
) -> Option<(Relay, cell_protocol::Gen)> {
    loop {
        match Relay::start(session).await {
            Ok(relay) => return Some(relay),
            Err(err) => tracing::warn!("{err:#}; retrying"),
        }

        tokio::select! {
            () = tokio::time::sleep(REGISTER_RETRY) => {}
            _ = shutdown.wait_for(|&stopping| stopping) => return None,
        }
    }
}

/// The single fallback handler: match the request path to a route, guard it,
/// and hand the request to the route's cell.
async fn handler(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_owned();

    let Some(route) = routes::match_route(&state.routes, &path) else {
        return (
            StatusCode::NOT_FOUND,
            "myrmic gateway: no application is mounted at this path\n",
        )
            .into_response();
    };

    let req = match guard(&state, &route, req).await {
        Ok(req) => req,
        Err(response) => return response,
    };

    let (mut parts, body) = req.into_parts();
    let Some(method) = wire_method(&parts.method) else {
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n").into_response();
    };
    // Present only when the client asked for one; the cell decides whether to
    // accept it.
    let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &state)
        .await
        .ok();
    let Ok(body) = axum::body::to_bytes(body, MAX_HTTP_BODY).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large\n").into_response();
    };

    let ctx = Uuid::new_v4();
    let request = HttpRequest {
        gateway: state.relay.sri(),
        ctx,
        method,
        path: strip_mount(&route.mount, &path),
        query: parts.uri.query().map(str::to_owned),
        headers: wire_headers(&parts.headers),
        body: body.to_vec(),
    };

    let (reply, _pending) = state.relay.expect_reply(ctx);
    if let Err(err) = state.relay.send(&route.owner, &route.http, &request).await {
        tracing::warn!("request {ctx} for {}: {err:#}", route.mount);
        return (StatusCode::BAD_GATEWAY, "bad gateway\n").into_response();
    }

    let mut shutdown = state.shutdown.clone();
    let reply = tokio::select! {
        reply = tokio::time::timeout(REPLY_TIMEOUT, reply) => match reply {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => return (StatusCode::SERVICE_UNAVAILABLE, "unavailable\n").into_response(),
            Err(_) => {
                tracing::warn!("request {ctx} for {}: no reply from {}", route.mount, route.owner);
                return (StatusCode::GATEWAY_TIMEOUT, "gateway timeout\n").into_response();
            }
        },
        _ = shutdown.wait_for(|&stopping| stopping) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "shutting down\n").into_response();
        }
    };

    answer(&state, &route, &parts, &path, upgrade, ctx, reply).await
}

/// Applies the route's OIDC guard, if the routing config gives it one.
async fn guard(state: &AppState, route: &GatewayRoute, req: Request) -> Result<Request, Response> {
    let Some(routing) = state.routing.as_ref() else {
        return Ok(req);
    };

    let oidc = {
        // Fail closed: without the config we cannot tell whether this route
        // is guarded, so we must not serve it as if it were public.
        let Ok(guard) = routing.read() else {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "myrmic gateway: routing configuration unavailable\n",
            )
                .into_response());
        };
        guard
            .routes
            .as_ref()
            .and_then(|map| map.get(&route.mount).and_then(|c| c.oidc.clone()))
    };

    match oidc {
        Some(oidc) => oidc::apply_oidc(&state.http_client, req, oidc).await,
        None => Ok(req),
    }
}

/// Turns the cell's reply into the client's response.
async fn answer(
    state: &AppState,
    route: &GatewayRoute,
    parts: &request::Parts,
    path: &str,
    upgrade: Option<WebSocketUpgrade>,
    ctx: Uuid,
    Reply { reply, sender }: Reply,
) -> Response {
    // Assets and sockets belong to whoever replied, which may not be the owner.
    let replier = || {
        sender.ok_or_else(|| {
            tracing::warn!("request {ctx}: reply carries no sender");
            (StatusCode::BAD_GATEWAY, "bad gateway\n").into_response()
        })
    };

    match reply {
        HttpReply::Response(response) => into_response(response),
        HttpReply::Asset(asset) => {
            let cell = match replier() {
                Ok(cell) => cell,
                Err(response) => return response,
            };
            if let Some(mut location) = canonical_redirect(&route.mount, &parts.method, path) {
                if let Some(query) = parts.uri.query() {
                    location.push('?');
                    location.push_str(query);
                }
                return Redirect::temporary(&location).into_response();
            }
            assets::serve(&state.session, cell, asset).await
        }
        HttpReply::Upgrade { callback } => {
            let cell = match replier() {
                Ok(cell) => cell,
                Err(response) => return response,
            };
            let Some(upgrade) = upgrade else {
                return (StatusCode::UPGRADE_REQUIRED, "upgrade required\n").into_response();
            };
            // `Command`'s serde derive doesn't validate; the name is a cell's.
            let Ok(callback) = Command::try_from(callback.as_ref()) else {
                tracing::warn!("request {ctx}: invalid upgrade callback from {cell}");
                return (StatusCode::BAD_GATEWAY, "bad gateway\n").into_response();
            };

            let relay = state.relay.clone();
            let shutdown = state.shutdown.clone();
            let alive = state.sockets_alive.clone();
            upgrade
                .on_upgrade(move |socket| {
                    ws::run(relay, cell, callback, ctx, socket, shutdown, alive)
                })
                .into_response()
        }
    }
}

fn into_response(response: HttpResponse) -> Response {
    let Ok(status) = StatusCode::from_u16(response.status.into_u16()) else {
        tracing::warn!(
            "cell replied with invalid status {}",
            response.status.into_u16()
        );
        return (StatusCode::BAD_GATEWAY, "bad gateway\n").into_response();
    };

    let mut out = Response::new(Body::from(response.body));
    *out.status_mut() = status;
    for header in response.headers {
        match (
            HeaderName::try_from(header.name.as_str()),
            HeaderValue::try_from(header.value.as_str()),
        ) {
            (Ok(name), Ok(value)) => {
                out.headers_mut().append(name, value);
            }
            _ => tracing::warn!("cell replied with invalid header '{}'", header.name),
        }
    }
    out
}

fn wire_method(method: &Method) -> Option<wire::Method> {
    Some(match *method {
        Method::OPTIONS => wire::Method::Options,
        Method::GET => wire::Method::Get,
        Method::POST => wire::Method::Post,
        Method::PUT => wire::Method::Put,
        Method::DELETE => wire::Method::Delete,
        Method::HEAD => wire::Method::Head,
        Method::TRACE => wire::Method::Trace,
        Method::CONNECT => wire::Method::Connect,
        Method::PATCH => wire::Method::Patch,
        _ => return None,
    })
}

/// Headers whose values are valid UTF-8; the rest are dropped. The gateway's
/// session cookie is the client's OIDC login, so it never reaches a cell.
fn wire_headers(headers: &HeaderMap) -> Vec<wire::Header> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let mut value = value.to_str().ok()?.to_owned();
            if name == header::COOKIE {
                value = strip_session_cookie(&value)?;
            }
            Some(wire::Header {
                name: name.as_str().to_owned(),
                value,
            })
        })
        .collect()
}

/// `cookies` without [`SESSION_COOKIE`], or `None` if nothing is left.
fn strip_session_cookie(cookies: &str) -> Option<String> {
    let kept: Vec<&str> = cookies
        .split(';')
        .map(str::trim)
        .filter(|pair| {
            !pair.is_empty() && pair.split('=').next().map(str::trim) != Some(SESSION_COOKIE)
        })
        .collect();

    (!kept.is_empty()).then(|| kept.join("; "))
}

/// Whether an asset request for a bare mount root should be redirected to the
/// same path with a trailing slash, and to where.
///
/// The redirect exists so relative asset URLs in the served document resolve
/// under the mount. Only for `GET`/`HEAD` navigations, and never for the `/`
/// catch-all (already a directory).
fn canonical_redirect(mount: &str, method: &Method, path: &str) -> Option<String> {
    if mount == "/" {
        return None;
    }
    if method != Method::GET && method != Method::HEAD {
        return None;
    }
    (path == mount).then(|| format!("{path}/"))
}

/// Strips the mount prefix, yielding the path relative to the app (always
/// starting with `/`).
fn strip_mount(mount: &str, path: &str) -> String {
    if mount == "/" {
        return assets::with_leading_slash(path);
    }
    let rest = &path[mount.len()..];
    if rest.is_empty() {
        "/".to_owned()
    } else {
        rest.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_stripping() {
        assert_eq!(strip_mount("/todos", "/todos"), "/");
        assert_eq!(strip_mount("/todos", "/todos/"), "/");
        assert_eq!(
            strip_mount("/todos", "/todos/assets/app.js"),
            "/assets/app.js"
        );
        assert_eq!(strip_mount("/", "/assets/app.js"), "/assets/app.js");
        assert_eq!(strip_mount("/", "/"), "/");
    }

    #[test]
    fn trailing_slash_redirect() {
        // Bare mount root → redirect to the slash form.
        assert_eq!(
            canonical_redirect("/chat", &Method::GET, "/chat"),
            Some("/chat/".to_owned())
        );
        assert_eq!(
            canonical_redirect("/chat", &Method::HEAD, "/chat"),
            Some("/chat/".to_owned())
        );
        // Already canonical, or a nested asset path → no redirect.
        assert_eq!(canonical_redirect("/chat", &Method::GET, "/chat/"), None);
        assert_eq!(
            canonical_redirect("/chat", &Method::GET, "/chat/index-abc.js"),
            None
        );
        // Non-navigation methods and the catch-all are exempt.
        assert_eq!(canonical_redirect("/chat", &Method::POST, "/chat"), None);
        assert_eq!(canonical_redirect("/", &Method::GET, "/"), None);
    }

    #[test]
    fn response_conversion() {
        let response = HttpResponse::new(myrmic_common::types::web::StatusCode::from_u16(201))
            .header("content-type", "text/plain")
            .header("bad header", "x")
            .body("hi");
        let out = into_response(response);

        assert_eq!(out.status(), StatusCode::CREATED);
        assert_eq!(out.headers()["content-type"], "text/plain");
        assert_eq!(out.headers().len(), 1);
    }

    #[test]
    fn headers_keep_utf8_values_only() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ok", HeaderValue::from_static("yes"));
        headers.insert("x-bin", HeaderValue::from_bytes(b"\xff").unwrap());

        let wire = wire_headers(&headers);
        assert_eq!(
            wire,
            vec![wire::Header {
                name: "x-ok".to_owned(),
                value: "yes".to_owned()
            }]
        );
    }

    #[test]
    fn session_cookie_never_reaches_cells() {
        assert_eq!(
            strip_session_cookie("theme=dark; myrmic_gateway_session=abc; lang=en").as_deref(),
            Some("theme=dark; lang=en")
        );
        assert_eq!(strip_session_cookie("myrmic_gateway_session=abc"), None);
        // Only an exact name match is the session cookie.
        assert_eq!(
            strip_session_cookie("myrmic_gateway_session_x=1").as_deref(),
            Some("myrmic_gateway_session_x=1")
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("myrmic_gateway_session=abc"),
        );
        assert!(wire_headers(&headers).is_empty());
    }
}
