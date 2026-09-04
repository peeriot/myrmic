//! The myrmic socket gateway.
//!
//! A native process that joins the swarm as a peer and exposes a socket
//! (HTTP / `WebSocket`) entrypoint into the network. External clients reach the
//! swarm through it: static web assets are served straight out of the blob
//! store, and a `WebSocket`/HTTP API maps requests onto cell commands and
//! events.
//!
//! Cells declare how they should be served in the `gateway-config` datalayer
//! registry (see [`sorg_common::gateway_config`]). Every gateway discovers and
//! watches that registry and builds a routing table from it, so all gateways in
//! the network serve the same routes.
//!
//! This crate provides the server ([`run`]); the `myrmic gateway` CLI command
//! is the primary way to launch it.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use cell_protocol::{PlacementEntry, PlacementKind, Sri};
use db_client::v1::models::{Scope, path_resolve};
use futures_util::{SinkExt, Stream, StreamExt, stream};
use myrmic_common::cells::{Command as CellCommand, Event as CellEvent};
use serde::Deserialize;
use sorg_common::gateway_config::{AssetConfig, Fallback};
use sorg_common::{Mailbox, claim_placement, placement_exists, remove_placement};
use tokio::time::Instant;
use tower_sessions::cookie::time;
use uuid::Uuid;
use zenoh::Session;

use crate::api::{ClientMessage, ServerMessage};
use crate::sessions::Sessions;

pub mod api;
pub mod oidc;
pub mod routes;
pub mod sessions;

const DEFAULT_SESSION_INACTIVITY_TIMER_SECS: i64 = 120;
const MINIMUM_SESSION_INACTIVITY_TIMER_SECS: i64 = 2;

/// The port the gateway binds to when none is configured.
pub const DEFAULT_PORT: u16 = 8080;

/// How long an HTTP session's SRI is kept alive after its last SSE stream
/// disconnects, so an `EventSource` auto-reconnect can re-attach to the same
/// identity (and pending mailbox) instead of minting a fresh one.
const HTTP_SESSION_GRACE: Duration = Duration::from_secs(30);

/// Maximum accepted body size for a `POST` to the HTTP cell API (1 `MiB`).
const MAX_HTTP_BODY: usize = 1 << 20;

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
    sessions: sessions::Sessions,

    routing: Option<RoutingView>,

    http_client: reqwest::Client,
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

    let (routes, discovery) = routes::spawn_discovery(&session, routing.as_ref());
    let (sessions, reaper) = sessions::spawn_session_reaper(&session);

    // Redirects are the caller's business: the OIDC flow inspects the provider's
    // 3xx responses itself.
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("unable to build the gateway's http client")?;

    let state = AppState {
        session,
        routes,
        sessions,
        routing,
        http_client,
    };

    let sessions = tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default())
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
        .with_graceful_shutdown(shutdown)
        .await
        .context("gateway server error");

    discovery.abort();
    reaper.abort();

    result?;
    tracing::info!("gateway stopped");
    Ok(())
}

/// The single fallback handler: match the request path to an application, then
/// either hand off to its cell API (not yet implemented) or serve a static
/// asset from its blob-store scope.
async fn handler(State(state): State<AppState>, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();

    // verify srn
    // verify application base

    let Some(route) = routes::match_route(&state.routes, &path) else {
        return (
            StatusCode::NOT_FOUND,
            "myrmic gateway: no application is mounted at this path\n",
        )
            .into_response();
    };

    let req = if let Some(routing) = state.routing.as_ref() {
        let oidc = {
            // Fail closed: without the config we cannot tell whether this route
            // is guarded, so we must not serve it as if it were public.
            let Ok(guard) = routing.read() else {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "myrmic gateway: routing configuration unavailable\n",
                )
                    .into_response();
            };
            guard
                .routes
                .as_ref()
                .and_then(|map| map.get(&route.mount).and_then(|c| c.oidc.clone()))
        };

        if let Some(oidc) = oidc {
            match oidc::apply_oidc(&state.http_client, req, oidc).await {
                Ok(req) => req,
                Err(err) => {
                    return err;
                }
            }
        } else {
            req
        }
    } else {
        req
    };

    // Canonicalize a bare mount root to a trailing slash (the standard
    // static-server directory redirect). The document lives at the mount, but
    // its relative asset URLs (`./index.js`) only resolve under the mount when
    // the browser's base ends in `/` — otherwise it treats the last segment as
    // a file and resolves them above the mount.
    if let Some(mut location) =
        canonical_redirect(route.assets.is_some(), &route.mount, &method, &path)
    {
        if let Some(query) = req.uri().query() {
            location.push('?');
            location.push_str(query);
        }
        return Redirect::temporary(&location).into_response();
    }

    let rel = strip_mount(&route.mount, &path);

    // WebSocket upgrade path → the bidirectional cell API.
    if route.ws.as_deref().is_some_and(|p| path_is_under(&rel, p)) {
        return ws_upgrade(state, route.owner, req).await;
    }

    // HTTP API path → the same per-session cell API as WebSocket, method-
    // multiplexed on the single `api` path: GET opens the SSE receive stream,
    // POST sends one fire-and-forget command/event.
    if route.api.as_deref().is_some_and(|p| path_is_under(&rel, p)) {
        if method == Method::GET || method == Method::HEAD {
            return sse_stream(state, req).await;
        }
        if method == Method::POST {
            return http_send(state, route.owner, req).await;
        }
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n").into_response();
    }

    // Everything else is a static asset.
    if method != Method::GET && method != Method::HEAD {
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n").into_response();
    }

    match &route.assets {
        Some(assets) => serve_asset(&state.session, assets, &rel).await,
        None => (StatusCode::NOT_FOUND, "not found\n").into_response(),
    }
}

/// Whether a request for a bare mount root should be redirected to the same
/// path with a trailing slash, and to where.
///
/// Only asset-serving mounts need it (the redirect exists so relative asset
/// URLs in the served document resolve under the mount), only for `GET`/`HEAD`
/// navigations, and never for the `/` catch-all (already a directory).
fn canonical_redirect(
    has_assets: bool,
    mount: &str,
    method: &Method,
    path: &str,
) -> Option<String> {
    if !has_assets || mount == "/" {
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
        return with_leading_slash(path);
    }
    let rest = &path[mount.len()..];
    if rest.is_empty() {
        "/".to_owned()
    } else {
        rest.to_owned()
    }
}

/// Whether the app-relative path `rel` falls under the API sub-path `api`.
fn path_is_under(rel: &str, api: &str) -> bool {
    let api = with_leading_slash(api);
    rel == api || rel.starts_with(&format!("{api}/"))
}

/// Resolves and serves a static asset for the app-relative path `rel`.
async fn serve_asset(session: &Session, assets: &AssetConfig, rel: &str) -> Response {
    let index = assets.index.as_deref();

    // The file to fetch: directories (and the root) resolve to the index.
    let primary = if rel == "/" || rel.is_empty() {
        index.map(with_leading_slash)
    } else if rel.ends_with('/') {
        index.map(|i| format!("{rel}{}", i.trim_start_matches('/')))
    } else {
        Some(rel.to_owned())
    };

    let Some(primary) = primary else {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    };

    match fetch_blob(session, &assets.scope, &primary).await {
        Ok(Some(bytes)) => return asset_response(&primary, bytes),
        Ok(None) => {}
        Err(err) => return unavailable(&err),
    }

    // SPA fallback: when enabled, a path that looks like a client-side route
    // (no file extension) falls back to the index document.
    if assets.fallback == Fallback::Spa
        && let Some(index) = index
        && !looks_like_file(&primary)
    {
        let index = with_leading_slash(index);
        match fetch_blob(session, &assets.scope, &index).await {
            Ok(Some(bytes)) => return asset_response(&index, bytes),
            Ok(None) => {}
            Err(err) => return unavailable(&err),
        }
    }

    (StatusCode::NOT_FOUND, "not found\n").into_response()
}

/// Fetches a blob by path from the given scope, or `None` if it doesn't exist.
async fn fetch_blob(
    session: &Session,
    scope: &Scope,
    path: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let db = db_client::v1::Client::new(session);
    let scope = scope.clone();
    let path = with_leading_slash(path);

    let response = db
        .read_tx_in(scope.clone(), async move |client, tx_id| {
            client
                .send(path_resolve::Request {
                    id: tx_id,
                    op: path_resolve::Op {
                        scope,
                        path,
                        range: None,
                    },
                })
                .await
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to communicate with db: {err}"))?
        .map_err(|err| anyhow::anyhow!("path resolve failed: {}", err.message))?;

    Ok(response.blob.map(|blob| blob.blob))
}

/// Builds a 200 response for a served asset, inferring the MIME from its path.
fn asset_response(path: &str, bytes: Vec<u8>) -> Response {
    let content_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
}

fn unavailable(err: &anyhow::Error) -> Response {
    tracing::warn!("asset lookup failed: {err}");
    (StatusCode::SERVICE_UNAVAILABLE, "upstream unavailable\n").into_response()
}

/// Ensures a path has exactly one leading slash.
fn with_leading_slash(path: &str) -> String {
    format!("/{}", path.trim_start_matches('/'))
}

/// Heuristic: does the last path segment have a file extension?
fn looks_like_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'))
}

// ---- cell command/event API (per-session SRI) --------------------------------

/// Upgrades a request on the route's `ws` path to a `WebSocket` carrying
/// `ClientMessage`/`ServerMessage`. `owner` is the cell that declared the
/// route — the default target for messages that name none.
async fn ws_upgrade(state: AppState, owner: Sri, req: Request) -> Response {
    let (mut parts, _body) = req.into_parts();
    match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws
            .on_upgrade(move |socket| handle_ws(state, owner, socket))
            .into_response(),
        Err(rejection) => rejection.into_response(),
    }
}

/// Bridges one `WebSocket` session to the swarm.
///
/// Mints a per-session SRI, announces it as `Ready`, forwards inbound
/// `ClientMessage`s as fire-and-forget cell commands/events (stamped with the
/// session SRI as sender, and addressed to `owner` when they name no target),
/// and streams the session's mailbox back as `ServerMessage`s.
async fn handle_ws(state: AppState, owner: Sri, socket: WebSocket) {
    let session_uuid = Uuid::new_v4();
    let session_sri = Sri::from_uuid(session_uuid);
    tracing::debug!("ws session up: {session_sri}");

    // Register the session as a placeholder cell so cells can deliver replies
    // to its SRI — the host rejects commands addressed to unknown cells.
    if let Err(err) = claim_placement(
        &state.session,
        PlacementEntry {
            sri: session_sri,
            kind: PlacementKind::Placeholder,
            app: None,
            gen_id: cell_protocol::Gen::from_timestamp(&state.session.new_timestamp()),
        },
    )
    .await
    {
        tracing::warn!("failed to register session {session_sri}: {err}");
    }

    let (mut sink, mut stream) = socket.split();

    // Announce the session's SRI so the client (and, through it, cells) know
    // the address replies are delivered to.
    let ready = serde_json::to_string(&ServerMessage::Ready {
        session: session_uuid.to_string(),
    })
    .unwrap_or_default();
    if sink.send(Message::Text(ready.into())).await.is_err() {
        return;
    }

    // Pump: the session mailbox → client. Shares its core (subscribe → drain →
    // map to `ServerMessage`) with the SSE transport via `session_replies`.
    let pump = {
        let session = state.session.clone();
        let sri = session_sri;
        async move {
            let mut replies = std::pin::pin!(session_replies(session, sri));
            while let Some(message) = replies.next().await {
                let json = serde_json::to_string(&message).unwrap_or_default();
                if sink.send(Message::Text(json.into())).await.is_err() {
                    return;
                }
            }
        }
    };

    // Read: client → cell.
    let read = {
        let session = state.session.clone();
        async move {
            while let Some(Ok(frame)) = stream.next().await {
                let parsed = match frame {
                    Message::Text(text) => serde_json::from_str::<ClientMessage>(text.as_str()),
                    Message::Binary(bytes) => {
                        serde_json::from_slice::<ClientMessage>(bytes.as_ref())
                    }
                    Message::Close(_) => break,
                    _ => continue,
                };

                match parsed {
                    Ok(message) => {
                        if let Err(err) = dispatch(&session, session_uuid, owner, message).await {
                            tracing::warn!("session {session_uuid} dispatch failed: {err}");
                        }
                    }
                    Err(err) => tracing::warn!("session {session_uuid} bad client message: {err}"),
                }
            }
        }
    };

    tokio::select! {
        () = pump => {}
        () = read => {}
    }

    tracing::debug!("ws session down: {session_sri}");
    // Best-effort cleanup: deregister the session and clear its mailbox.
    let _ = remove_placement(&state.session, &session_sri).await;
    let _ = Mailbox::new(&state.session)
        .drain_commands(session_sri)
        .await;
}

/// Sends one client message into the swarm as a fire-and-forget command/event,
/// stamped with the session SRI as the sender so cells can reply to it.
///
/// A command naming no target goes to `owner`, the cell that declared the route
/// the message arrived on, so a front end talking only to its own cell never
/// has to discover an SRI.
async fn dispatch(
    session: &Session,
    session_uuid: Uuid,
    owner: Sri,
    message: ClientMessage,
) -> anyhow::Result<()> {
    let mailbox = Mailbox::new(session);
    match message {
        ClientMessage::Command { sri, name, payload } => {
            let target_sri = match &sri {
                Some(sri) => {
                    Sri::from_target(sri).map_err(|e| anyhow!("invalid target '{sri}': {e}"))?
                }
                None => owner,
            };

            if !placement_exists(session, &target_sri)
                .await
                .map_err(|e| anyhow!("placement check failed: {e}"))?
            {
                bail!("cell '{target_sri}' has no placement");
            }

            let command = CellCommand::try_from(name.as_str())
                .map_err(|_| anyhow!("invalid command name '{name}'"))?;
            mailbox
                .send_command(&target_sri, &command, Some(payload), Some(session_uuid))
                .await
                .map_err(|e| anyhow!("failed to send command: {e}"))?;
        }
        ClientMessage::Event { name, payload } => {
            let event = CellEvent::try_from(name.as_str())
                .map_err(|_| anyhow!("invalid event name '{name}'"))?;
            mailbox
                .publish_event(&event, Some(payload), Some(session_uuid))
                .await
                .map_err(|e| anyhow!("failed to publish event: {e}"))?;
        }
    }
    Ok(())
}

/// A stream of a session's cell replies (its inbound mailbox), shared by the
/// `WebSocket` and SSE transports.
///
/// Spawns a task that subscribes to the session's mailbox, drains it on every
/// change (with a periodic backstop for missed pokes), and forwards each queued
/// command as a [`ServerMessage`] over a channel. The task lives until the
/// returned stream is dropped — at which point the receiver closes, the send
/// fails, and the subscription is released.
/// How many replies one mailbox read pulls in.
const REPLY_BATCH_SIZE: usize = 16;

fn session_replies(session: Session, sri: Sri) -> impl Stream<Item = ServerMessage> {
    let (tx, rx) = tokio::sync::mpsc::channel::<ServerMessage>(32);

    tokio::spawn(async move {
        let mut commands = Mailbox::new(&session)
            .commands(sri, Duration::from_secs(2), REPLY_BATCH_SIZE)
            .await;

        while let Some(incoming) = commands.next().await {
            let command = incoming.command();
            let message = ServerMessage::Command {
                sri: command.attachment.sender().map(|u| u.to_string()),
                name: command.cmd.as_ref().to_string(),
                payload: command.payload.clone().unwrap_or_default(),
            };

            // Consume first; if that fails the command stays queued and
            // redelivers, so don't forward it (avoids a duplicate).
            if let Err(err) = incoming.consume().await {
                tracing::warn!("session {sri} reply consume failed: {err}");
                continue;
            }

            if tx.send(message).await.is_err() {
                return; // the consumer dropped the stream
            }
        }
    });

    stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|m| (m, rx)) })
}

/// Opens an SSE receive stream for an HTTP session.
///
/// A fresh connection mints a new session SRI (registered as a placeholder cell
/// so replies can be delivered) and announces it as the first `Ready` event. A
/// connection carrying a known token — via `?token=` or the `Last-Event-ID`
/// header that `EventSource` resends on auto-reconnect — re-attaches to the
/// existing session within its grace window instead. The client echoes the
/// session id in `X-Gateway-Session` on its `POST`s.
async fn sse_stream(state: AppState, req: Request) -> Response {
    let session_uuid = match reattach_token(&req) {
        Some(id) if sessions::attach_stream(&state.sessions, id) => {
            tracing::debug!("http session reattached: {id}");
            id
        }
        _ => {
            let id = Uuid::new_v4();
            let sri = Sri::from_uuid(id);
            if let Err(err) = claim_placement(
                &state.session,
                PlacementEntry {
                    sri,
                    kind: PlacementKind::Placeholder,
                    app: None,
                    gen_id: cell_protocol::Gen::from_timestamp(&state.session.new_timestamp()),
                },
            )
            .await
            {
                tracing::warn!("failed to register http session {id}: {err}");
            }
            sessions::register(&state.sessions, id);
            tracing::debug!("http session up: {id}");
            id
        }
    };

    let session_sri = Sri::from_uuid(session_uuid);
    let id = session_uuid.to_string();

    // Decrements the stream count (and arms the grace countdown) when the client
    // disconnects — the guard is dropped with the response stream below.
    let guard = StreamGuard {
        sessions: state.sessions.clone(),
        id: session_uuid,
    };

    let ready = stream::once({
        let id = id.clone();
        async move {
            Ok::<Event, Infallible>(sse_event(
                &ServerMessage::Ready {
                    session: id.clone(),
                },
                &id,
            ))
        }
    });
    let replies = session_replies(state.session.clone(), session_sri)
        .map(move |message| Ok::<Event, Infallible>(sse_event(&message, &id)));

    // Carry the guard in the stream's state so it drops exactly when the client
    // goes away (stream exhausted or dropped).
    let body = stream::unfold(
        (Box::pin(ready.chain(replies)), guard),
        |(mut body, guard)| async move { body.next().await.map(|item| (item, (body, guard))) },
    );

    Sse::new(body)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Sends one `POST`ed [`ClientMessage`] into the swarm as a fire-and-forget
/// command/event, stamped with the caller's session SRI so cells can reply to
/// it, and addressed to `owner` when the message names no target.
async fn http_send(state: AppState, owner: Sri, req: Request) -> Response {
    let Some(session_uuid) = req
        .headers()
        .get("x-gateway-session")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
    else {
        return (
            StatusCode::UNAUTHORIZED,
            "missing or malformed X-Gateway-Session header\n",
        )
            .into_response();
    };

    if !sessions::touch(&state.sessions, session_uuid) {
        return (StatusCode::UNAUTHORIZED, "unknown or expired session\n").into_response();
    }

    let Ok(bytes) = axum::body::to_bytes(req.into_body(), MAX_HTTP_BODY).await else {
        return (StatusCode::BAD_REQUEST, "unable to read request body\n").into_response();
    };
    let message = match serde_json::from_slice::<ClientMessage>(&bytes) {
        Ok(message) => message,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid client message: {err}\n"),
            )
                .into_response();
        }
    };

    match dispatch(&state.session, session_uuid, owner, message).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(err) => {
            tracing::warn!("http session {session_uuid} dispatch failed: {err}");
            let body = serde_json::to_string(&ServerMessage::Error {
                message: err.to_string(),
            })
            .unwrap_or_default();
            (
                StatusCode::BAD_GATEWAY,
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
    }
}

/// Builds an SSE event for one `ServerMessage`. The `id` is the session token,
/// so a browser resends it as `Last-Event-ID` on auto-reconnect. No `event:`
/// name is set, so the browser dispatches it to the default `onmessage` handler
/// carrying the same tagged JSON the `WebSocket` transport emits.
fn sse_event(message: &ServerMessage, id: &str) -> Event {
    Event::default()
        .id(id)
        .data(serde_json::to_string(message).unwrap_or_default())
}

/// Extracts a re-attach token from a stream request: the `?token=` query
/// parameter takes precedence over the `Last-Event-ID` header.
fn reattach_token(req: &Request) -> Option<Uuid> {
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("token=")
                && let Ok(id) = Uuid::parse_str(value)
            {
                return Some(id);
            }
        }
    }
    req.headers()
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
}

/// Decrements a session's stream count when an SSE response is dropped, arming
/// the grace countdown once the last stream goes away.
struct StreamGuard {
    sessions: Sessions,
    id: Uuid,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.sessions.lock()
            && let Some(state) = map.get_mut(&self.id)
        {
            state.active_streams = state.active_streams.saturating_sub(1);
            if state.active_streams == 0 {
                state.deadline = Instant::now() + HTTP_SESSION_GRACE;
            }
        }
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
    fn api_sub_path() {
        assert!(path_is_under("/api", "/api"));
        assert!(path_is_under("/api/todos", "/api"));
        // The API sub-path is normalized to a leading slash.
        assert!(path_is_under("/api/todos", "api"));
        assert!(!path_is_under("/apix", "/api"));
        assert!(!path_is_under("/", "/api"));
    }

    #[test]
    fn trailing_slash_redirect() {
        // Bare mount root serving assets → redirect to the slash form.
        assert_eq!(
            canonical_redirect(true, "/chat", &Method::GET, "/chat"),
            Some("/chat/".to_owned())
        );
        assert_eq!(
            canonical_redirect(true, "/chat", &Method::HEAD, "/chat"),
            Some("/chat/".to_owned())
        );
        // Already canonical, or a nested asset path → no redirect.
        assert_eq!(
            canonical_redirect(true, "/chat", &Method::GET, "/chat/"),
            None
        );
        assert_eq!(
            canonical_redirect(true, "/chat", &Method::GET, "/chat/index-abc.js"),
            None
        );
        // Non-navigation methods, non-asset mounts, and the catch-all are exempt.
        assert_eq!(
            canonical_redirect(true, "/chat", &Method::POST, "/chat"),
            None
        );
        assert_eq!(
            canonical_redirect(false, "/chat", &Method::GET, "/chat"),
            None
        );
        assert_eq!(canonical_redirect(true, "/", &Method::GET, "/"), None);
    }

    #[test]
    fn file_heuristic() {
        assert!(looks_like_file("/assets/app.js"));
        assert!(looks_like_file("/index.html"));
        assert!(!looks_like_file("/dashboard"));
        assert!(!looks_like_file("/users/42"));
        assert!(!looks_like_file("/"));
    }

    #[test]
    fn leading_slash() {
        assert_eq!(with_leading_slash("index.html"), "/index.html");
        assert_eq!(with_leading_slash("/index.html"), "/index.html");
        assert_eq!(with_leading_slash("///x"), "/x");
    }

    #[test]
    fn reattach_token_sources() {
        use axum::body::Body;

        let want = Uuid::new_v4();

        // `?token=` query parameter.
        let req = axum::http::Request::builder()
            .uri(format!("/chat/api?token={want}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(reattach_token(&req), Some(want));

        // `Last-Event-ID` header — what EventSource resends on auto-reconnect.
        let req = axum::http::Request::builder()
            .uri("/chat/api")
            .header("last-event-id", want.to_string())
            .body(Body::empty())
            .unwrap();
        assert_eq!(reattach_token(&req), Some(want));

        // The query parameter wins over the header.
        let other = Uuid::new_v4();
        let req = axum::http::Request::builder()
            .uri(format!("/chat/api?token={want}"))
            .header("last-event-id", other.to_string())
            .body(Body::empty())
            .unwrap();
        assert_eq!(reattach_token(&req), Some(want));

        // Absent or malformed → no token (a fresh session is minted).
        let req = axum::http::Request::builder()
            .uri("/chat/api")
            .body(Body::empty())
            .unwrap();
        assert_eq!(reattach_token(&req), None);
        let req = axum::http::Request::builder()
            .uri("/chat/api?token=not-a-uuid")
            .body(Body::empty())
            .unwrap();
        assert_eq!(reattach_token(&req), None);
    }
}
