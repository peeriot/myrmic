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
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use cell_protocol::{PlacementEntry, PlacementKind, Sri};
use db_client::v1::models::{Scope, path_resolve};
use futures_util::{SinkExt, Stream, StreamExt, stream};
use myrmic_common::cells::{Command as CellCommand, Event as CellEvent};
use serde::Deserialize;
use sorg_common::gateway_config::{AssetConfig, Fallback, GatewayRoute};
use sorg_common::{Mailbox, claim_placement, placement_exists, remove_placement};
use tower_sessions::cookie::{Cookie, SameSite, time};
use uuid::Uuid;
use zenoh::Session;

use crate::api::{ClientMessage, ServerMessage};
use crate::sessions::{Secret, Sessions};

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

/// How many `myrmic_gw` cookie values a request may present; the rest are
/// ignored. Bounds the work done under the `Sessions` lock.
const MAX_PRESENTED_BINDINGS: usize = 8;

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

    /// Serve mounts without `oidc` to anyone, with full API access; routes
    /// marked `anonymous: true` stay owner-only. Local development only; logs
    /// a warning at startup.
    #[serde(default)]
    pub anonymous: bool,

    /// Which discovered routes to serve, and the OIDC provider guarding each -
    /// either inline, or a path to a JSON file holding the same. `None` serves
    /// every route that is discovered, but refuses each one with 403 unless
    /// [`Config::anonymous`] is set.
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

impl RoutingConfig {
    fn has_guarded_route(&self) -> bool {
        self.routes.as_ref().is_some_and(|routes| {
            routes
                .values()
                .any(|route| route.oidc.is_some() || route.anonymous)
        })
    }
}

/// How this gateway treats one mount.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RouteConfig {
    /// The only cell allowed to own this mount. `None` accepts whichever cell
    /// claimed it first.
    #[serde(default)]
    pub srn: Option<String>,

    /// The provider guarding this mount. Without it (and without
    /// `anonymous`) the mount is refused, unless the gateway runs with
    /// `--anonymous`.
    #[serde(default)]
    pub oidc: Option<OidcConfig>,

    /// Serve this mount without a login. Its API then only reaches the
    /// mount's owner cell, and clients may not publish events through it.
    #[serde(default)]
    pub anonymous: bool,
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

    anonymous: bool,

    over_https: bool,

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
        anonymous,
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

    if anonymous {
        tracing::warn!(
            "gateway: --anonymous / gateway.anonymous is set; mounts without `oidc` are served to anyone, without a login"
        );
    } else if routing.is_none() {
        tracing::warn!(
            "gateway: no routing config and no --anonymous / gateway.anonymous; every mount answers 403 until a route has `oidc` or `anonymous: true`"
        );
    } else if routing.as_ref().is_some_and(|routing| {
        routing
            .read()
            .is_ok_and(|config| !config.has_guarded_route())
    }) {
        tracing::warn!(
            "gateway: no route has `oidc` or `anonymous: true` and no --anonymous / gateway.anonymous; every mount answers 403"
        );
    }

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
        anonymous,
        over_https,
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

    // Fail closed: a login, `anonymous: true` (owner only) or `--anonymous`, else 403.
    let (oidc, owner_only) = match state.routing.as_ref() {
        Some(routing) => {
            // Without the config we cannot tell whether this route is
            // guarded, so we must not serve it as if it were public.
            let Ok(guard) = routing.read() else {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "myrmic gateway: routing configuration unavailable\n",
                )
                    .into_response();
            };
            let entry = guard.routes.as_ref().and_then(|map| map.get(&route.mount));

            (
                entry.and_then(|config| config.oidc.clone()),
                entry.is_some_and(|config| config.anonymous),
            )
        }
        None => (None, false),
    };

    let req = if let Some(oidc) = oidc {
        match oidc::apply_oidc(&state.http_client, req, oidc).await {
            Ok(req) => req,
            Err(err) => return err,
        }
    } else if owner_only || state.anonymous {
        req
    } else {
        tracing::info!(
            "gateway: refusing {method} {path}: mount {} has no `oidc`; set `anonymous: true` on the route or start the gateway with --anonymous",
            route.mount
        );
        return (
            StatusCode::FORBIDDEN,
            "myrmic gateway: forbidden: login required\n",
        )
            .into_response();
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
        return ws_upgrade(state, route.owner, owner_only, req).await;
    }

    // HTTP API path → the same per-session cell API as WebSocket, method-
    // multiplexed on the single `api` path: GET opens the SSE receive stream,
    // POST sends one fire-and-forget command/event.
    if route.api.as_deref().is_some_and(|p| path_is_under(&rel, p)) {
        if method == Method::GET || method == Method::HEAD {
            return sse_stream(state, &route, req).await;
        }
        if method == Method::POST {
            return http_send(state, &route, owner_only, req).await;
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
/// route - the default target for messages that name none. `owner_only` limits
/// commands to `owner` and refuses client events (an anonymous mount).
async fn ws_upgrade(state: AppState, owner: Sri, owner_only: bool, req: Request) -> Response {
    let (mut parts, _body) = req.into_parts();
    match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws
            .on_upgrade(move |socket| handle_ws(state, owner, owner_only, socket))
            .into_response(),
        Err(rejection) => rejection.into_response(),
    }
}

/// Bridges one `WebSocket` session to the swarm.
///
/// Mints a per-session SRI, announces it as `Ready`, forwards inbound
/// `ClientMessage`s as fire-and-forget cell commands/events (stamped with the
/// session SRI as sender, and addressed to `owner` when they name no target),
/// and streams the session's mailbox back as `ServerMessage`s. `owner_only`
/// limits commands to `owner` and refuses client events (an anonymous mount).
async fn handle_ws(state: AppState, owner: Sri, owner_only: bool, socket: WebSocket) {
    let session_uuid = Uuid::new_v4();
    let session_sri = Sri::from_uuid(session_uuid);
    tracing::debug!("ws session up: {session_sri}");

    // Register the session as a placeholder cell so cells can deliver replies
    // to its SRI — the host rejects commands addressed to unknown cells.
    let gen_id = cell_protocol::Gen::from_timestamp(&state.session.new_timestamp());
    if let Err(err) = claim_placement(
        &state.session,
        PlacementEntry {
            sri: session_sri,
            kind: PlacementKind::Placeholder,
            app: None,
            gen_id,
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
        token: None,
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
                        if let Err(err) =
                            dispatch(&session, session_uuid, owner, owner_only, message).await
                        {
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
    let _ = remove_placement(&state.session, &session_sri, gen_id).await;
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
///
/// On an anonymous mount (`owner_only`), a command may only target `owner` and
/// events are refused, so a page served without login cannot reach the rest of
/// the swarm.
async fn dispatch(
    session: &Session,
    session_uuid: Uuid,
    owner: Sri,
    owner_only: bool,
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

            if owner_only && target_sri != owner {
                bail!(
                    "anonymous mount: commands may only target the mount owner '{owner}', not '{target_sri}'"
                );
            }

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
            if owner_only {
                bail!("anonymous mount: clients may not publish events");
            }

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
/// connection carrying the token in the `Last-Event-ID` header that
/// `EventSource` resends on auto-reconnect re-attaches to the existing session
/// within its grace window instead, but only when it also presents the
/// session's binding cookie on the mount and for the owner it was minted on.
/// The client echoes the token, not the session id, in `X-Gateway-Session` on
/// its `POST`s.
async fn sse_stream(state: AppState, route: &GatewayRoute, req: Request) -> Response {
    let presented = presented_bindings(&req, cookie_name(state.over_https));
    let reattached = reattach_token(&req).and_then(|token| {
        sessions::attach_stream(
            &state.sessions,
            &token,
            &presented,
            &route.mount,
            route.owner,
        )
    });
    let mut set_cookie = None;
    let (session_uuid, token) = match reattached {
        Some((session_uuid, token)) => {
            tracing::debug!(
                "http session reattached: {} on {}",
                Sri::from_uuid(session_uuid),
                route.mount
            );
            (session_uuid, token)
        }
        None => {
            let id = Uuid::new_v4();
            let sri = Sri::from_uuid(id);
            let gen_id = cell_protocol::Gen::from_timestamp(&state.session.new_timestamp());
            if let Err(err) = claim_placement(
                &state.session,
                PlacementEntry {
                    sri,
                    kind: PlacementKind::Placeholder,
                    app: None,
                    gen_id,
                },
            )
            .await
            {
                tracing::warn!("failed to register http session {id}: {err}");
            }
            let (token, binding) = sessions::register(
                &state.sessions,
                id,
                gen_id,
                &presented,
                &route.mount,
                route.owner,
            );
            set_cookie = Some(binding);
            tracing::debug!("http session up: {sri} on {}", route.mount);
            (id, token)
        }
    };

    let session_sri = Sri::from_uuid(session_uuid);
    let id = token.expose().to_owned();

    // Decrements the stream count (and arms the grace countdown) when the client
    // disconnects — the guard is dropped with the response stream below.
    let guard = StreamGuard {
        sessions: state.sessions.clone(),
        token: token.clone(),
    };

    let ready = stream::once({
        let id = id.clone();
        async move {
            Ok::<Event, Infallible>(sse_event(
                &ServerMessage::Ready {
                    session: session_uuid.to_string(),
                    token: Some(token),
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

    let mut response = Sse::new(body)
        .keep_alive(KeepAlive::default())
        .into_response();
    if let Some(binding) = set_cookie {
        match HeaderValue::try_from(binding_cookie(&binding, state.over_https)) {
            Ok(value) => {
                response.headers_mut().append(header::SET_COOKIE, value);
            }
            Err(err) => tracing::warn!("binding cookie is not a valid header value: {err}"),
        }
    }

    response
}

/// Sends one `POST`ed [`ClientMessage`] into the swarm as a fire-and-forget
/// command/event, stamped with the caller's session SRI so cells can reply to
/// it, and addressed to `owner` when the message names no target. `owner_only`
/// limits commands to `owner` and refuses client events (an anonymous mount).
async fn http_send(
    state: AppState,
    route: &GatewayRoute,
    owner_only: bool,
    req: Request,
) -> Response {
    let session_uuid = match authorize(&state, &route.mount, route.owner, &req) {
        Ok(session_uuid) => session_uuid,
        Err(refused) => return refused,
    };

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

    match dispatch(
        &state.session,
        session_uuid,
        route.owner,
        owner_only,
        message,
    )
    .await
    {
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

/// Checks a `POST`'s session and returns the SRI to send as. Exactly one
/// `X-Gateway-Session` header is accepted; every refusal is [`session_refused`].
fn authorize(state: &AppState, mount: &str, owner: Sri, req: &Request) -> Result<Uuid, Response> {
    let mut values = req.headers().get_all("x-gateway-session").iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(session_refused());
    };
    let Ok(token) = value.to_str() else {
        return Err(session_refused());
    };
    let presented = presented_bindings(req, cookie_name(state.over_https));

    sessions::touch(&state.sessions, token, &presented, mount, owner).ok_or_else(session_refused)
}

/// The one response for every refused session.
fn session_refused() -> Response {
    (StatusCode::UNAUTHORIZED, "unknown or expired session\n").into_response()
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

/// Extracts a re-attach token from a stream request: the `Last-Event-ID`
/// header that `EventSource` resends on auto-reconnect. Never the URL.
fn reattach_token(req: &Request) -> Option<String> {
    req.headers()
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// The browser binding cookie's name; `__Host-` pins it to this host over HTTPS.
fn cookie_name(over_https: bool) -> &'static str {
    if over_https {
        "__Host-myrmic_gw"
    } else {
        "myrmic_gw"
    }
}

/// The first `MAX_PRESENTED_BINDINGS` values of cookie `name`, in request
/// order, across every `Cookie` header.
fn presented_bindings(req: &Request, name: &str) -> Vec<String> {
    req.headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(Cookie::split_parse)
        .filter_map(Result::ok)
        .filter(|cookie| cookie.name() == name)
        .map(|cookie| cookie.value().to_owned())
        .take(MAX_PRESENTED_BINDINGS)
        .collect()
}

/// The `Set-Cookie` value carrying `binding`. All attributes are constants.
fn binding_cookie(binding: &Secret, over_https: bool) -> String {
    Cookie::build((cookie_name(over_https), binding.expose().to_owned()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(over_https)
        .build()
        .to_string()
}

/// Decrements a session's stream count when an SSE response is dropped, arming
/// the grace countdown once the last stream goes away.
struct StreamGuard {
    sessions: Sessions,
    token: Secret,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        if let Ok(mut table) = self.sessions.lock() {
            table.release_stream(&self.token);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;
    use std::str::from_utf8;

    use axum::body::{Body, BodyDataStream, to_bytes};
    use cell_protocol::Gen;
    use serde_json::Value;
    use sorg_common::gateway_config::GatewayRoute;
    use tokio::time::{Instant, timeout};

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
    fn reattach_token_sources() -> anyhow::Result<()> {
        // `?token=` is ignored: a credential never comes from the URL.
        let req = Request::builder()
            .uri("/chat/api?token=abc")
            .body(Body::empty())?;
        assert_eq!(reattach_token(&req), None);

        // `Last-Event-ID`, what EventSource resends on auto-reconnect.
        let req = Request::builder()
            .uri("/chat/api?token=other")
            .header("last-event-id", "abc")
            .body(Body::empty())?;
        assert_eq!(reattach_token(&req), Some("abc".to_owned()));

        // Absent: a fresh session is minted.
        let req = Request::builder().uri("/chat/api").body(Body::empty())?;
        assert_eq!(reattach_token(&req), None);

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mount_access_without_login() -> anyhow::Result<()> {
        let unguarded = RoutingConfig {
            base_url: None,
            routes: Some(HashMap::from([(
                "/chat".to_owned(),
                RouteConfig::default(),
            )])),
        };
        // (case, routing, --anonymous, method, path, expected status)
        let cases = [
            // No routing, no --anonymous: the whole mount is refused.
            (
                "no routing GET api",
                None,
                false,
                Method::GET,
                "/chat/api",
                StatusCode::FORBIDDEN,
            ),
            (
                "no routing POST api",
                None,
                false,
                Method::POST,
                "/chat/api",
                StatusCode::FORBIDDEN,
            ),
            (
                "no routing GET ws",
                None,
                false,
                Method::GET,
                "/chat/ws",
                StatusCode::FORBIDDEN,
            ),
            (
                "no routing GET root",
                None,
                false,
                Method::GET,
                "/chat/",
                StatusCode::FORBIDDEN,
            ),
            // --anonymous: served as before (ws extractor rejects a non-upgrade GET).
            (
                "--anonymous GET ws",
                None,
                true,
                Method::GET,
                "/chat/ws",
                StatusCode::BAD_REQUEST,
            ),
            (
                "--anonymous GET root",
                None,
                true,
                Method::GET,
                "/chat/",
                StatusCode::NOT_FOUND,
            ),
            // Routing entry without oidc and without anonymous: refused.
            (
                "unguarded route POST api",
                Some(unguarded.clone()),
                false,
                Method::POST,
                "/chat/api",
                StatusCode::FORBIDDEN,
            ),
            (
                "unguarded route GET ws",
                Some(unguarded),
                false,
                Method::GET,
                "/chat/ws",
                StatusCode::FORBIDDEN,
            ),
            // Routing file without routes: nothing is guarded, all refused.
            (
                "routes None POST api",
                Some(RoutingConfig::default()),
                false,
                Method::POST,
                "/chat/api",
                StatusCode::FORBIDDEN,
            ),
            // Routing entry marked anonymous: served.
            (
                "anonymous route GET ws",
                Some(anonymous_route()),
                false,
                Method::GET,
                "/chat/ws",
                StatusCode::BAD_REQUEST,
            ),
        ];

        for (case, routing, anonymous, method, path, expected) in cases {
            let state = AppState::for_test(routing, anonymous).await?;
            let response =
                handler(State(state), request(method.clone(), path, None, None, "")?).await;
            assert_eq!(
                response.status(),
                expected,
                "{case}: {method} {path}, --anonymous={anonymous}"
            );
        }

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_refusals_are_uniform() -> anyhow::Result<()> {
        let state = AppState::for_test(None, true).await?;
        let event = r#"{"type":"event","name":"x"}"#;
        let seeded = Seeded::new(&state)?;
        let post = |token: Option<&str>, cookie: Option<&str>, body: &str| {
            request(Method::POST, "/chat/api", token, cookie, body)
        };
        let (token, cookie) = (seeded.token.expose(), Some(seeded.cookie.as_str()));

        // Positive control: the valid header and cookie get past the check and
        // refresh only the deadline of their own session.
        let before = seeded.snapshot(&state);
        let (status, set_cookie, _) = send(&state, post(Some(token), cookie, event)?).await?;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(set_cookie, None);
        let touched = seeded.snapshot(&state);
        assert!(touched[0] > before[0]);
        assert_eq!(touched[1..], before[1..]);

        let before = touched;
        let mut twice = post(Some(token), cookie, event)?;
        twice
            .headers_mut()
            .append("x-gateway-session", token.parse()?);
        let mut refused = vec![
            ("missing header".to_owned(), post(None, cookie, event)?),
            ("two headers".to_owned(), twice),
            (
                "unknown token".to_owned(),
                post(Some(Secret::random().expose()), cookie, event)?,
            ),
            (
                "bad body, no header".to_owned(),
                post(None, cookie, "not json")?,
            ),
            (
                "mount mismatch".to_owned(),
                post(
                    Some(seeded.mount_token.expose()),
                    Some(&seeded.mount_cookie),
                    event,
                )?,
            ),
            (
                "owner mismatch".to_owned(),
                post(
                    Some(seeded.owner_token.expose()),
                    Some(&seeded.owner_cookie),
                    event,
                )?,
            ),
            (
                "binding mismatch".to_owned(),
                post(Some(token), Some(&seeded.random_cookie), event)?,
            ),
            ("missing cookie".to_owned(), post(Some(token), None, event)?),
            (
                "empty cookie".to_owned(),
                post(Some(token), Some("myrmic_gw="), event)?,
            ),
            (
                "another session's binding".to_owned(),
                post(Some(token), Some(&seeded.mount_cookie), event)?,
            ),
        ];
        for (case, value) in seeded.hostile_values() {
            let mut req = post(None, cookie, event)?;
            req.headers_mut()
                .insert("x-gateway-session", HeaderValue::from_bytes(&value)?);
            refused.push((format!("header {case}"), req));
        }

        for (case, req) in refused {
            let (status, set_cookie, body) = send(&state, req).await?;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{case}");
            assert_eq!(set_cookie, None, "{case}");
            assert_eq!(body, "unknown or expired session\n", "{case}");
            assert_eq!(seeded.snapshot(&state), before, "{case}");
        }

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_last_event_id_mints_a_fresh_session() -> anyhow::Result<()> {
        let state = AppState::for_test(None, true).await?;
        let seeded = Seeded::new(&state)?;
        let token = seeded.token.expose().as_bytes();
        let (cookie, random) = (Some(&seeded.cookie), Some(&seeded.random_cookie));
        let mut reattach = vec![
            (
                "missing cookie".to_owned(),
                None,
                token.to_vec(),
                seeded.sri,
            ),
            (
                "binding mismatch".to_owned(),
                random,
                token.to_vec(),
                seeded.sri,
            ),
            (
                "mount mismatch".to_owned(),
                Some(&seeded.mount_cookie),
                seeded.mount_token.expose().into(),
                seeded.mount_sri,
            ),
            (
                "owner mismatch".to_owned(),
                Some(&seeded.owner_cookie),
                seeded.owner_token.expose().into(),
                seeded.owner_sri,
            ),
        ];
        for (case, value) in seeded.hostile_values() {
            reattach.push((format!("id {case}"), cookie, value, seeded.sri));
        }
        let before = seeded.snapshot(&state);

        for (case, cookie, id, target) in reattach {
            let opened =
                open_stream(&state, "/chat/api", cookie.map(String::as_str), Some(&id)).await?;
            assert_ne!(opened.session, target.to_string(), "{case}");
            assert_eq!(seeded.snapshot(&state), before, "{case}");
        }

        // Positive control: the valid token and cookie do re-attach.
        let opened =
            open_stream(&state, "/chat/api", cookie.map(String::as_str), Some(token)).await?;
        assert_eq!(opened.session, seeded.sri.to_string());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sse_token_is_not_the_sri() -> anyhow::Result<()> {
        let state = AppState::for_test(None, true).await?;
        let owner = chat_owner()?;
        let opened = open_stream(&state, "/chat/api", None, None).await?;
        let cookie = cookie_pair(opened.set_cookie.as_deref())?;
        let binding = cookie.trim_start_matches("myrmic_gw=").to_owned();

        assert_eq!(opened.token.len(), 43);
        assert!(Uuid::parse_str(&opened.token).is_err());
        assert_ne!(opened.token, opened.session);
        assert_eq!(opened.id, opened.token);
        assert_eq!(
            sessions::touch(&state.sessions, &opened.token, &[binding], "/chat", owner),
            Some(Uuid::parse_str(&opened.session)?)
        );

        // The token gets past the check; the SRI does not.
        let event = r#"{"type":"event","name":"x"}"#;
        let (status, _, body) = send(
            &state,
            request(
                Method::POST,
                "/chat/api",
                Some(&opened.token),
                Some(&cookie),
                event,
            )?,
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("failed to publish event:"), "{body}");
        let (status, _, _) = send(
            &state,
            request(
                Method::POST,
                "/chat/api",
                Some(&opened.session),
                Some(&cookie),
                event,
            )?,
        )
        .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reattach_keeps_session_and_token() -> anyhow::Result<()> {
        let state = AppState::for_test(None, true).await?;
        let first = open_stream(&state, "/chat/api", None, None).await?;
        let cookie = cookie_pair(first.set_cookie.as_deref())?;
        let again = open_stream(
            &state,
            "/chat/api",
            Some(&cookie),
            Some(first.token.as_bytes()),
        )
        .await?;

        assert_eq!(again.session, first.session);
        assert_eq!(again.token, first.token);
        assert_eq!(again.set_cookie, None);
        assert_eq!(
            stream_state(&state, &first.token).map(|(streams, _)| streams),
            Some(2)
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn any_of_the_first_eight_cookies_matches() -> anyhow::Result<()> {
        let state = AppState::for_test(None, true).await?;
        let event = r#"{"type":"event","name":"x"}"#;
        let (_, token, binding) = seed(&state, "/chat", chat_owner()?);
        let right = format!("myrmic_gw={}", binding.expose());
        let other = format!("myrmic_gw={}", Secret::random().expose());

        for cookie in [format!("{other}; {right}"), format!("{right}; {other}")] {
            let (status, _, _) = send(
                &state,
                request(
                    Method::POST,
                    "/chat/api",
                    Some(token.expose()),
                    Some(&cookie),
                    event,
                )?,
            )
            .await?;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{cookie}");
        }

        // The right value at position 9 is never looked at.
        let ninth = [other.as_str(); 8].join("; ") + "; " + &right;
        let (status, _, _) = send(
            &state,
            request(
                Method::POST,
                "/chat/api",
                Some(token.expose()),
                Some(&ninth),
                event,
            )?,
        )
        .await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        Ok(())
    }

    #[test]
    fn presented_bindings_keeps_the_first_eight() -> anyhow::Result<()> {
        let values: Vec<String> = (1..=9).map(|n| format!("v{n}")).collect();
        let pairs = |range: Range<usize>| {
            values
                .iter()
                .skip(range.start)
                .take(range.len())
                .map(|value| format!("myrmic_gw={value}"))
                .collect::<Vec<_>>()
                .join("; ")
        };
        let req = Request::builder()
            .header(header::COOKIE, format!("other=x; =; {}", pairs(0..5)))
            .header(header::COOKIE, pairs(5..9))
            .body(Body::empty())?;

        let presented = presented_bindings(&req, "myrmic_gw");
        assert_eq!(presented.len(), MAX_PRESENTED_BINDINGS);
        assert_eq!(
            presented,
            values.iter().take(8).cloned().collect::<Vec<_>>()
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn binding_cookie_attributes() -> anyhow::Result<()> {
        let mut state = AppState::for_test(None, true).await?;
        for (over_https, name) in [(false, "myrmic_gw"), (true, "__Host-myrmic_gw")] {
            state.over_https = over_https;
            let opened = open_stream(&state, "/chat/api", None, None).await?;
            let cookie = Cookie::parse(
                opened
                    .set_cookie
                    .clone()
                    .ok_or_else(|| anyhow!("no Set-Cookie"))?,
            )?;

            assert_eq!(cookie.name(), name);
            assert_eq!(cookie.value().len(), 43);
            assert_eq!(cookie.path(), Some("/"));
            assert_eq!(cookie.http_only(), Some(true));
            assert_eq!(cookie.same_site(), Some(SameSite::Strict));
            assert_eq!(cookie.secure().unwrap_or(false), over_https);
            assert_eq!(cookie.domain(), None);
            assert_eq!(cookie.max_age(), None);
            assert_eq!(cookie.expires(), None);
        }

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mint_reuses_only_a_live_binding() -> anyhow::Result<()> {
        let state = AppState::for_test(None, true).await?;
        let event = r#"{"type":"event","name":"x"}"#;
        let (_, token, binding) = seed(&state, "/chat", chat_owner()?);
        let live = format!("myrmic_gw={}", binding.expose());
        let garbage = format!("myrmic_gw={}", "x".repeat(4000));
        let dead = format!("myrmic_gw={}", Secret::random().expose());

        let reused = open_stream(&state, "/chat/api", Some(&live), None).await?;
        assert_eq!(cookie_pair(reused.set_cookie.as_deref())?, live);

        // Both streams post with the one cookie jar.
        assert_ne!(reused.token, token.expose());
        for token in [token.expose(), &reused.token] {
            let (status, _, _) = send(
                &state,
                request(Method::POST, "/chat/api", Some(token), Some(&live), event)?,
            )
            .await?;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
        }

        // Not live or oversized: a fresh value.
        for cookie in [garbage, dead.clone()] {
            let opened = open_stream(&state, "/chat/api", Some(&cookie), None).await?;
            let minted = cookie_pair(opened.set_cookie.as_deref())?;
            assert_ne!(minted, live);
            assert_ne!(minted, dead);
        }

        Ok(())
    }

    #[test]
    fn mistyped_anonymous_is_rejected() {
        let route =
            serde_json::from_str::<RoutingConfig>(r#"{"routes": {"/chat": {"anonymous": "yes"}}}"#);
        let config = serde_json::from_str::<Config>(r#"{"anonymous": "yes"}"#);

        for error in [route.err(), config.err()] {
            assert!(error.is_some_and(|err| err.to_string().contains("expected a boolean")));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn anonymous_mount_reaches_only_its_owner() -> anyhow::Result<()> {
        let owner = Sri::of_path("chat-owner").map_err(|err| anyhow!("srn: {err}"))?;
        let other = Sri::of_path("someone-else").map_err(|err| anyhow!("srn: {err}"))?;
        let owner_refusal = format!(
            "anonymous mount: commands may only target the mount owner '{owner}', not '{other}'"
        );
        let owner_name = "chat-owner";
        let owner_uuid = owner.to_string();
        let event_refusal = "anonymous mount: clients may not publish events";

        // A route marked anonymous stays owner-only even with --anonymous set.
        for gateway_anonymous in [false, true] {
            let state = AppState::for_test(Some(anonymous_route()), gateway_anonymous).await?;

            let (status, message) = post(
                &state,
                r#"{"type":"command","sri":"someone-else","name":"x"}"#,
            )
            .await?;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(message, owner_refusal);

            let (status, message) = post(&state, r#"{"type":"event","name":"x"}"#).await?;
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(message, event_refusal);

            // The owner itself, or a command without a target, gets past the
            // guard, to the placement lookup.
            let owner_bodies = [owner_name.to_owned(), owner_uuid.clone()]
                .map(|target| format!(r#"{{"type":"command","sri":"{target}","name":"x"}}"#))
                .into_iter()
                .chain([r#"{"type":"command","name":"x"}"#.to_owned()]);
            for body in owner_bodies {
                let (_, message) = post(&state, &body).await?;
                assert!(message.starts_with("placement check failed:"), "{message}");
            }
        }

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gateway_wide_anonymous_keeps_events() -> anyhow::Result<()> {
        let state = AppState::for_test(None, true).await?;

        // Past the guard the publish itself fails: the test has no db.
        let (status, message) = post(&state, r#"{"type":"event","name":"x"}"#).await?;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(message.starts_with("failed to publish event:"), "{message}");

        Ok(())
    }

    impl AppState {
        /// A gateway state over a loopback zenoh session (no scouting, no
        /// peers) serving one route, `/chat`, owned by `chat-owner`, with
        /// `api` at `/api` and `ws` at `/ws` and no assets.
        async fn for_test(routing: Option<RoutingConfig>, anonymous: bool) -> anyhow::Result<Self> {
            let mut config = zenoh::Config::default();
            config
                .insert_json5("scouting/multicast/enabled", "false")
                .map_err(|err| anyhow!("scouting config: {err}"))?;
            config
                .insert_json5("timestamping/enabled", "{ peer: true }")
                .map_err(|err| anyhow!("timestamping config: {err}"))?;
            let session = zenoh::open(config)
                .await
                .map_err(|err| anyhow!("unable to open session: {err}"))?;

            let route = GatewayRoute {
                owner: Sri::of_path("chat-owner").map_err(|err| anyhow!("srn: {err}"))?,
                mount: "/chat".to_owned(),
                assets: None,
                api: Some("/api".to_owned()),
                ws: Some("/ws".to_owned()),
            };

            Ok(Self {
                session,
                routes: Arc::new(RwLock::new(vec![route])),
                sessions: Sessions::default(),
                routing: routing.map(|routing| Arc::new(RwLock::new(routing))),
                http_client: reqwest::Client::new(),
                anonymous,
                over_https: false,
            })
        }
    }

    fn request(
        method: Method,
        path: &str,
        session: Option<&str>,
        cookie: Option<&str>,
        body: &str,
    ) -> anyhow::Result<Request> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(session) = session {
            builder = builder.header("x-gateway-session", session);
        }
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }

        Ok(builder.body(Body::from(body.to_owned()))?)
    }

    /// Runs one request through `handler`, bounded, and returns status,
    /// `Set-Cookie` and body.
    async fn send(
        state: &AppState,
        req: Request,
    ) -> anyhow::Result<(StatusCode, Option<String>, String)> {
        let response = timeout(Duration::from_secs(10), handler(State(state.clone()), req)).await?;
        let status = response.status();
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .map(|value| value.to_str().map(str::to_owned))
            .transpose()?;
        let bytes = to_bytes(response.into_body(), MAX_HTTP_BODY).await?;

        Ok((status, set_cookie, String::from_utf8(bytes.to_vec())?))
    }

    /// The first frame of an SSE stream. `_body` keeps the stream, and with
    /// it the session's `StreamGuard`, alive until this value is dropped.
    struct Opened {
        set_cookie: Option<String>,
        session: String,
        token: String,
        id: String,
        _body: BodyDataStream,
    }

    /// Opens `GET path` with an optional `Cookie` and `Last-Event-ID`, and reads
    /// the first frame.
    async fn open_stream(
        state: &AppState,
        path: &str,
        cookie: Option<&str>,
        last_event_id: Option<&[u8]>,
    ) -> anyhow::Result<Opened> {
        let mut req = request(Method::GET, path, None, cookie, "")?;
        if let Some(id) = last_event_id {
            req.headers_mut()
                .insert("last-event-id", HeaderValue::from_bytes(id)?);
        }
        let response = timeout(Duration::from_secs(10), handler(State(state.clone()), req)).await?;
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .map(|value| value.to_str().map(str::to_owned))
            .transpose()?;
        let mut body = response.into_body().into_data_stream();
        let frame = timeout(Duration::from_secs(10), body.next())
            .await?
            .ok_or_else(|| anyhow!("stream ended before the first frame"))??;
        let frame = from_utf8(&frame)?;
        let field = |name: &str| {
            frame
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .map(|value| value.trim_start().to_owned())
                .ok_or_else(|| anyhow!("no {name} in {frame:?}"))
        };
        let id = field("id:")?;
        let ready = serde_json::from_str::<Value>(&field("data:")?)?;
        let text = |key: &str| {
            ready
                .get(key)
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned()
        };

        Ok(Opened {
            set_cookie,
            session: text("session"),
            token: text("token"),
            id,
            _body: body,
        })
    }

    /// A session on `/chat` with its cookie, one minted on another mount and
    /// one for another owner, and a cookie that is bound to nothing.
    struct Seeded {
        sri: Uuid,
        token: Secret,
        cookie: String,
        mount_sri: Uuid,
        mount_token: Secret,
        mount_cookie: String,
        owner_sri: Uuid,
        owner_token: Secret,
        owner_cookie: String,
        random_cookie: String,
    }

    impl Seeded {
        fn new(state: &AppState) -> anyhow::Result<Self> {
            let other = Sri::of_path("someone-else").map_err(|err| anyhow!("srn: {err}"))?;
            let (sri, token, binding) = seed(state, "/chat", chat_owner()?);
            let (mount_sri, mount_token, mount_binding) = seed(state, "/secure", chat_owner()?);
            let (owner_sri, owner_token, owner_binding) = seed(state, "/chat", other);
            let jar = |binding: &Secret| format!("myrmic_gw={}", binding.expose());

            Ok(Self {
                sri,
                token,
                cookie: jar(&binding),
                mount_sri,
                mount_token,
                mount_cookie: jar(&mount_binding),
                owner_sri,
                owner_token,
                owner_cookie: jar(&owner_binding),
                random_cookie: jar(&Secret::random()),
            })
        }

        /// Values a client could send as the token that must never match.
        fn hostile_values(&self) -> [(&'static str, Vec<u8>); 4] {
            [
                ("empty", Vec::new()),
                ("non-utf8", b"\xff".to_vec()),
                ("4 KiB", vec![b'a'; 4096]),
                ("session sri", self.sri.to_string().into_bytes()),
            ]
        }

        /// `active_streams` and `deadline` of the three sessions.
        fn snapshot(&self, state: &AppState) -> [Option<(u32, Instant)>; 3] {
            [&self.token, &self.mount_token, &self.owner_token]
                .map(|token| stream_state(state, token.expose()))
        }
    }

    /// Registers a session as `sse_stream` mints one; returns its SRI, token and
    /// binding.
    fn seed(state: &AppState, mount: &str, owner: Sri) -> (Uuid, Secret, Secret) {
        let sri = Uuid::new_v4();
        let gen_id = Gen::from_timestamp(&state.session.new_timestamp());
        let (token, binding) = sessions::register(&state.sessions, sri, gen_id, &[], mount, owner);

        (sri, token, binding)
    }

    /// `active_streams` and `deadline` of the session behind `token`.
    fn stream_state(state: &AppState, token: &str) -> Option<(u32, Instant)> {
        let table = state.sessions.lock().ok()?;

        table
            .get(token)
            .map(|session| (session.active_streams, session.deadline))
    }

    /// The `name=value` pair of a `Set-Cookie` header, as a browser sends it back.
    fn cookie_pair(set_cookie: Option<&str>) -> anyhow::Result<String> {
        let cookie = Cookie::parse(set_cookie.ok_or_else(|| anyhow!("no Set-Cookie"))?)?;

        Ok(format!("{}={}", cookie.name(), cookie.value()))
    }

    fn chat_owner() -> anyhow::Result<Sri> {
        Sri::of_path("chat-owner").map_err(|err| anyhow!("srn: {err}"))
    }

    /// The `/chat` mount marked `anonymous: true`.
    fn anonymous_route() -> RoutingConfig {
        RoutingConfig {
            base_url: None,
            routes: Some(HashMap::from([(
                "/chat".to_owned(),
                RouteConfig {
                    anonymous: true,
                    ..RouteConfig::default()
                },
            )])),
        }
    }

    /// Posts `body` to `/chat/api` with a fresh registered session and returns
    /// the status and the error message of the reply (empty on `202`). Bounded:
    /// past the guard, dispatch goes on to a placement lookup against a db that
    /// is not there.
    async fn post(state: &AppState, body: &str) -> anyhow::Result<(StatusCode, String)> {
        let (_, token, binding) = seed(state, "/chat", chat_owner()?);
        let cookie = format!("myrmic_gw={}", binding.expose());

        let response = timeout(
            Duration::from_secs(10),
            handler(
                State(state.clone()),
                request(
                    Method::POST,
                    "/chat/api",
                    Some(token.expose()),
                    Some(&cookie),
                    body,
                )?,
            ),
        )
        .await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), MAX_HTTP_BODY).await?;
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|reply| reply.get("message")?.as_str().map(str::to_owned))
            .unwrap_or_default();

        Ok((status, message))
    }
}
