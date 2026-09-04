//! IPC tap client: lazy connect, backoff, generation-checked virtual-handle table.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use crate::types::{ClientRead, ClientWrite};

/// Lazy-connecting tap client.
///
/// Virtual handles issued by this client start at 1; 0 is never returned.
/// On reconnect the generation counter increments; any lookup against a
/// handle from a previous generation returns `Unavailable` without a wire
/// call (SR-12).
///
/// Every operation completes or fails within [`TAP_CALL_TIMEOUT`]; a stalled
/// peer therefore cannot keep a cell — or any cell queued behind it on the
/// shared connection — waiting for longer than that.
pub struct TapClient {
    /// `None` means no socket is configured (S4 fail-closed): every operation
    /// reports `Unavailable` (D3) without attempting a connection.
    socket_path: Option<PathBuf>,
    inner: tokio::sync::Mutex<ClientInner>,
}

/// Initial backoff delay (ms).
const BACKOFF_INIT_MS: u64 = 250;
/// Maximum backoff delay (ms).
const BACKOFF_CAP_MS: u64 = 5_000;
/// Reconnect SLA: once the server is reachable we must connect within this
/// many seconds.  Asserted by the unit tests via `tokio::time::pause()`.
pub const RECONNECT_SLA_SECS: u64 = 10;

/// Upper bound on a single tap operation, from the moment a cell calls it to
/// the moment it gets an answer — covering the wait for the shared connection,
/// connection establishment, and the wait for the peer's response.
///
/// This is a tunable product setting, not a protocol constant: the peer is
/// trusted to be prompt, so the value only has to be short enough that a stalled
/// peer cannot keep other cells waiting indefinitely, and long enough that a
/// merely slow one is not cut off.  Every bound in this module derives from it,
/// so it is the single place to retune.
///
/// It must stay well inside [`RECONNECT_SLA_SECS`]: connection establishment is
/// bounded by the same value, so an attempt that burns the whole bound has to
/// leave the reconnect loop time to retry and still meet its SLA.
pub const TAP_CALL_TIMEOUT: Duration = Duration::from_secs(5);

struct ClientInner {
    conn: Option<Connection>,
    generation: u64,
    next_virtual_handle: u32,
    /// Maps `virtual_handle` → the tap it is bound to.
    ///
    /// Shared by every cell on the node: a name resolved by one cell is the
    /// same entry another cell's resolve of that name reuses.  One entry per
    /// distinct tap name and none outliving its generation, so the table stays
    /// within the number of taps the registry has.
    handles: std::collections::HashMap<u32, HandleEntry>,
    backoff_ms: u64,
}

/// Which server-side registry a handle belongs to. Taps and outlets are
/// separate registries with independent handle spaces, so a virtual handle
/// must never cross families (a tap read with an outlet handle would hit an
/// unrelated tap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandleFamily {
    Tap,
    Outlet,
}

/// What a virtual handle is bound to.
struct HandleEntry {
    /// Generation the handle was issued in.
    generation: u64,
    /// Handle the server issued for `name`, valid only on that generation's
    /// connection.
    server_handle: u32,
    /// Name the handle was resolved from, so a later resolve of the same name
    /// can find this entry instead of issuing a second handle for one tap.
    name: String,
    /// Registry the handle belongs to (tap or outlet).
    family: HandleFamily,
}

impl HandleEntry {
    /// Whether the entry can still reach a tap.  A connection reset issues a
    /// new generation and invalidates every server handle from the old one, so
    /// an entry only counts as live while its generation is the current one.
    fn is_live_in(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

struct Connection {
    reader: tokio::io::ReadHalf<tokio::net::UnixStream>,
    writer: tokio::io::WriteHalf<tokio::net::UnixStream>,
}

impl TapClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self::build(Some(socket_path))
    }

    /// Construct a client with no socket configured. Every tap operation
    /// fail-closes to `Unavailable` (D3) without attempting a connection.
    ///
    /// Used when the host cannot resolve a signal-layer socket path (S4:
    /// neither `/run/peeriot` writable nor `XDG_RUNTIME_DIR` set). The cell
    /// host must still run cells that do not use taps, so a missing socket
    /// path maps to "taps unavailable", never a hard runtime-setup failure.
    pub fn unavailable() -> Self {
        Self::build(None)
    }

    fn build(socket_path: Option<PathBuf>) -> Self {
        Self {
            socket_path,
            inner: tokio::sync::Mutex::new(ClientInner {
                conn: None,
                generation: 0,
                next_virtual_handle: 1, // 0 is never issued
                handles: std::collections::HashMap::new(),
                backoff_ms: BACKOFF_INIT_MS,
            }),
        }
    }

    /// Resolve a tap name to a virtual handle (≥1), or `None` if the name is
    /// longer than [`MAX_RESOLVE_NAME_LEN`], not found, the server is
    /// unreachable, or the call did not finish within [`TAP_CALL_TIMEOUT`].
    ///
    /// A name the shared table already holds a live handle for resolves to that
    /// handle, so however often the cells on a node resolve the same taps, the
    /// table holds one entry per tap.
    pub async fn resolve(&self, name: &str) -> Option<u32> {
        self.resolve_family(name, HandleFamily::Tap).await
    }

    /// Resolve an outlet name to a virtual handle (≥1) — the write-side mirror
    /// of [`resolve`](Self::resolve), against the server's outlet registry.
    pub async fn outlet_resolve(&self, name: &str) -> Option<u32> {
        self.resolve_family(name, HandleFamily::Outlet).await
    }

    async fn resolve_family(&self, name: &str, family: HandleFamily) -> Option<u32> {
        use crate::MAX_RESOLVE_NAME_LEN;

        // Refuse before taking the connection: a frame past the protocol bound
        // is rejected by the server's framer, which drops the connection this
        // client shares with every other caller and staleness-invalidates all
        // handles issued on it.
        if name.len() > MAX_RESOLVE_NAME_LEN {
            return None;
        }

        bounded(self.resolve_unbounded(name, family), None).await
    }

    async fn resolve_unbounded(&self, name: &str, family: HandleFamily) -> Option<u32> {
        use crate::types::{Request, Response};

        let mut inner = self.inner.lock().await;

        ensure_connected(&mut inner, self.socket_path.as_deref()).await?;

        // Only now is the generation settled: a reconnect has already released
        // what it superseded, so whatever the table still holds for this name is
        // reachable and can be handed back without a second handle for one tap.
        if let Some(vh) = live_handle_for_name(&inner, name, family) {
            return Some(vh);
        }

        let req = match family {
            HandleFamily::Tap => Request::TapResolve {
                name: name.to_owned(),
            },
            HandleFamily::Outlet => Request::OutletResolve {
                name: name.to_owned(),
            },
        };
        let resp = send_recv(&mut inner, &req).await?;

        match resp {
            Response::Handle { handle } => {
                let vh = inner.next_virtual_handle;
                inner.next_virtual_handle = vh.checked_add(1).unwrap_or(1);
                let entry = HandleEntry {
                    generation: inner.generation,
                    server_handle: handle,
                    name: name.to_owned(),
                    family,
                };
                inner.handles.insert(vh, entry);
                Some(vh)
            }
            _ => None,
        }
    }

    /// Read the retained value for a virtual handle.  Reports `Unavailable` if
    /// the call does not finish within [`TAP_CALL_TIMEOUT`].
    pub async fn read_retained(&self, vh: u32) -> ClientRead {
        bounded(self.read_retained_unbounded(vh), ClientRead::Unavailable).await
    }

    async fn read_retained_unbounded(&self, vh: u32) -> ClientRead {
        use crate::types::{Request, Response};
        let mut inner = self.inner.lock().await;

        let Some(server_handle) = resolve_virtual_handle(&inner, vh, HandleFamily::Tap) else {
            return ClientRead::Unavailable;
        };

        // S5: handle ensure_connected's Option result consistently with
        // resolve/list_len — return Unavailable if the connection cannot be made.
        let Some(()) = ensure_connected(&mut inner, self.socket_path.as_deref()).await else {
            return ClientRead::Unavailable;
        };

        let req = Request::TapReadRetained {
            handle: server_handle,
        };
        match send_recv(&mut inner, &req).await {
            Some(Response::Retained {
                timestamp_ms,
                bytes,
            }) => ClientRead::Value {
                timestamp_ms,
                bytes,
            },
            Some(Response::Empty) => ClientRead::Empty,
            _ => ClientRead::Unavailable,
        }
    }

    /// Take the next event for a virtual handle.  Reports `Unavailable` if the
    /// call does not finish within [`TAP_CALL_TIMEOUT`].
    pub async fn take_event(&self, vh: u32) -> ClientRead {
        bounded(self.take_event_unbounded(vh), ClientRead::Unavailable).await
    }

    async fn take_event_unbounded(&self, vh: u32) -> ClientRead {
        use crate::types::{Request, Response};
        let mut inner = self.inner.lock().await;

        let Some(server_handle) = resolve_virtual_handle(&inner, vh, HandleFamily::Tap) else {
            return ClientRead::Unavailable;
        };

        // S5: handle ensure_connected's Option result consistently.
        let Some(()) = ensure_connected(&mut inner, self.socket_path.as_deref()).await else {
            return ClientRead::Unavailable;
        };

        let req = Request::TapTakeEvent {
            handle: server_handle,
        };
        match send_recv(&mut inner, &req).await {
            Some(Response::Event { bytes }) => ClientRead::Value {
                timestamp_ms: 0,
                bytes,
            },
            Some(Response::Empty) => ClientRead::Empty,
            _ => ClientRead::Unavailable,
        }
    }

    /// Drain batch — always `Empty` (D1).
    #[allow(clippy::unused_async)]
    pub async fn drain_batch(&self, _vh: u32) -> ClientRead {
        ClientRead::Empty
    }

    /// Return the number of taps, or `None` if the call does not finish within
    /// [`TAP_CALL_TIMEOUT`].
    pub async fn list_len(&self) -> Option<u32> {
        bounded(self.list_len_unbounded(), None).await
    }

    async fn list_len_unbounded(&self) -> Option<u32> {
        use crate::types::{Request, Response};
        let mut inner = self.inner.lock().await;

        ensure_connected(&mut inner, self.socket_path.as_deref()).await?;

        let req = Request::TapListLen;
        match send_recv(&mut inner, &req).await? {
            Response::Count { count } => Some(count),
            _ => None,
        }
    }

    /// Return the name and kind of a tap by index, or `None` if the call does
    /// not finish within [`TAP_CALL_TIMEOUT`].
    pub async fn list_entry(&self, index: u32) -> Option<(String, u8)> {
        bounded(self.list_entry_unbounded(index), None).await
    }

    async fn list_entry_unbounded(&self, index: u32) -> Option<(String, u8)> {
        use crate::types::{Request, Response};
        let mut inner = self.inner.lock().await;

        ensure_connected(&mut inner, self.socket_path.as_deref()).await?;

        let req = Request::TapListEntry { index };
        match send_recv(&mut inner, &req).await? {
            Response::Entry { name, kind } => Some((name, kind)),
            _ => None,
        }
    }

    // ── Outlet operations (the write-side mirror of the tap methods) ─────────

    /// Write a command payload to an outlet virtual handle.  Reports
    /// `Unavailable` if the handle is stale/unknown, the server is unreachable,
    /// or the call does not finish within [`TAP_CALL_TIMEOUT`]; `Rejected` if
    /// the server refuses the payload (wrong declared type) — or, without a
    /// wire call, if the payload exceeds [`MAX_OUTLET_WRITE_LEN`]: an oversized
    /// frame would cost the shared connection, and a payload that large can
    /// never decode into an outlet command type anyway.
    pub async fn outlet_write(&self, vh: u32, bytes: Vec<u8>) -> ClientWrite {
        if bytes.len() > crate::MAX_OUTLET_WRITE_LEN {
            return ClientWrite::Rejected;
        }
        bounded(
            self.outlet_write_unbounded(vh, bytes),
            ClientWrite::Unavailable,
        )
        .await
    }

    async fn outlet_write_unbounded(&self, vh: u32, bytes: Vec<u8>) -> ClientWrite {
        use crate::types::{Request, Response};
        let mut inner = self.inner.lock().await;

        let Some(server_handle) = resolve_virtual_handle(&inner, vh, HandleFamily::Outlet) else {
            return ClientWrite::Unavailable;
        };

        let Some(()) = ensure_connected(&mut inner, self.socket_path.as_deref()).await else {
            return ClientWrite::Unavailable;
        };

        let req = Request::OutletWrite {
            handle: server_handle,
            bytes,
        };
        match send_recv(&mut inner, &req).await {
            Some(Response::Written) => ClientWrite::Ok,
            Some(Response::Rejected) => ClientWrite::Rejected,
            _ => ClientWrite::Unavailable,
        }
    }

    /// Return the number of outlets, or `None` if the server answers
    /// `Unsupported`, is unreachable, or the call times out.
    pub async fn outlet_list_len(&self) -> Option<u32> {
        bounded(self.outlet_list_len_unbounded(), None).await
    }

    async fn outlet_list_len_unbounded(&self) -> Option<u32> {
        use crate::types::{Request, Response};
        let mut inner = self.inner.lock().await;

        ensure_connected(&mut inner, self.socket_path.as_deref()).await?;

        let req = Request::OutletListLen;
        match send_recv(&mut inner, &req).await? {
            Response::Count { count } => Some(count),
            _ => None,
        }
    }

    /// Return the name and kind of an outlet by index, or `None` if the index
    /// is out of range, the server answers `Unsupported`, or the call times out.
    pub async fn outlet_list_entry(&self, index: u32) -> Option<(String, u8)> {
        bounded(self.outlet_list_entry_unbounded(index), None).await
    }

    async fn outlet_list_entry_unbounded(&self, index: u32) -> Option<(String, u8)> {
        use crate::types::{Request, Response};
        let mut inner = self.inner.lock().await;

        ensure_connected(&mut inner, self.socket_path.as_deref()).await?;

        let req = Request::OutletListEntry { index };
        match send_recv(&mut inner, &req).await? {
            Response::Entry { name, kind } => Some((name, kind)),
            _ => None,
        }
    }

    /// The declared wire type of the tap behind `vh` (swarm#1315), or `None`
    /// if the handle is stale/unknown, the server is unreachable, or the call
    /// times out.
    pub async fn tap_type_id(&self, vh: u32) -> Option<u32> {
        bounded(self.type_id_unbounded(vh, HandleFamily::Tap), None).await
    }

    /// The declared command type of the outlet behind `vh` (swarm#1315), or
    /// `None` — see [`tap_type_id`](Self::tap_type_id).
    pub async fn outlet_type_id(&self, vh: u32) -> Option<u32> {
        bounded(self.type_id_unbounded(vh, HandleFamily::Outlet), None).await
    }

    async fn type_id_unbounded(&self, vh: u32, family: HandleFamily) -> Option<u32> {
        use crate::types::{Request, Response};
        let mut inner = self.inner.lock().await;

        let server_handle = resolve_virtual_handle(&inner, vh, family)?;
        ensure_connected(&mut inner, self.socket_path.as_deref()).await?;

        let req = match family {
            HandleFamily::Tap => Request::TapTypeId {
                handle: server_handle,
            },
            HandleFamily::Outlet => Request::OutletTypeId {
                handle: server_handle,
            },
        };
        match send_recv(&mut inner, &req).await? {
            Response::TypeId { id } => Some(id),
            _ => None,
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Run a tap operation under [`TAP_CALL_TIMEOUT`], yielding `on_timeout` if the
/// bound expires.
///
/// The bound covers the whole operation, including acquiring the shared
/// connection lock, so what a caller observes is bounded no matter how many
/// other callers are queued ahead of it.  Expiry drops the operation's future,
/// which releases that lock: an operation waiting for the lock wrote nothing and
/// leaves nothing to clean up, while one that already has a request in flight is
/// torn down by `send_recv`'s guard.
async fn bounded<T>(op: impl Future<Output = T>, on_timeout: T) -> T {
    tokio::time::timeout(TAP_CALL_TIMEOUT, op)
        .await
        .unwrap_or(on_timeout)
}

/// Look up a virtual handle in the table.  Returns `None` if the handle is
/// unknown, belongs to the other registry family (a tap handle used as an
/// outlet handle or vice versa), or its generation is stale (connection was
/// reset since it was issued).
fn resolve_virtual_handle(inner: &ClientInner, vh: u32, family: HandleFamily) -> Option<u32> {
    inner
        .handles
        .get(&vh)
        .filter(|entry| entry.family == family && entry.is_live_in(inner.generation))
        .map(|entry| entry.server_handle)
}

/// Look up the virtual handle a live entry already holds for a name in the
/// given registry family (tap and outlet namespaces are independent).
///
/// Liveness is the same condition [`resolve_virtual_handle`] applies, so an
/// entry a later generation has superseded is never handed to a caller as if it
/// could still reach a tap: the caller would get a handle that only ever reports
/// `Unavailable`.
fn live_handle_for_name(inner: &ClientInner, name: &str, family: HandleFamily) -> Option<u32> {
    inner
        .handles
        .iter()
        .find(|(_, entry)| {
            entry.name == name && entry.family == family && entry.is_live_in(inner.generation)
        })
        .map(|(vh, _)| *vh)
}

/// Drop every entry the current generation supersedes.
///
/// Their server handles died with the connection they were issued on, so they
/// can answer nothing; without this the table would gain a full set of dead
/// entries on every reconnect.
fn release_superseded(inner: &mut ClientInner) {
    let generation = inner.generation;
    inner
        .handles
        .retain(|_, entry| entry.is_live_in(generation));
}

/// Ensure the connection is live.  Returns `Some(())` on success, `None` if
/// the server is unreachable (no retry here — the caller decides).
async fn ensure_connected(inner: &mut ClientInner, path: Option<&std::path::Path>) -> Option<()> {
    if inner.conn.is_some() {
        return Some(());
    }
    // No socket configured (S4 fail-closed) → permanently unavailable (D3),
    // without a connect attempt.
    let path = path?;
    try_connect(inner, path).await
}

/// Attempt a single connection attempt.  Does NOT retry; updates the
/// generation if it makes a new connection.
///
/// Establishment carries its own [`TAP_CALL_TIMEOUT`] rather than relying on the
/// caller's: `connect_with_backoff` connects outside any tap operation and holds
/// the shared lock while it does, so a peer that accepts the socket and then
/// never completes the handshake would otherwise wedge the client for good.
async fn try_connect(inner: &mut ClientInner, path: &std::path::Path) -> Option<()> {
    let conn = tokio::time::timeout(TAP_CALL_TIMEOUT, connect_and_handshake(path))
        .await
        .ok()
        .flatten()?;

    // New generation — all old virtual handles are stale, and a stale entry is
    // dead weight in a table that has to stay bounded, so release them here:
    // this is the only place the generation changes.
    inner.generation = inner.generation.wrapping_add(1);
    release_superseded(inner);
    inner.conn = Some(conn);
    inner.backoff_ms = BACKOFF_INIT_MS;
    Some(())
}

/// Open the socket and complete the version handshake.
async fn connect_and_handshake(path: &std::path::Path) -> Option<Connection> {
    use crate::PROTOCOL_VERSION;
    use crate::framing::{decode_frame, read_frame, write_frame};
    use crate::types::{Request, Response};

    let stream = tokio::net::UnixStream::connect(path).await.ok()?;
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Send Hello
    write_frame(
        &mut writer,
        &Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await
    .ok()?;

    // Read HelloOk / HelloRejected
    let frame = read_frame(&mut reader).await.ok()?;
    let resp: Response = decode_frame(&frame).ok()?;

    match resp {
        Response::HelloOk { .. } => {}
        _ => return None, // Version rejected or unexpected
    }

    Some(Connection { reader, writer })
}

/// Send a request and receive a response.  On any error, tears down the
/// connection so the next call triggers a reconnect.
///
/// A drop-guard ensures that if this future is cancelled between the write and
/// the read — the request bytes are on the wire but the response has not been
/// received — the connection is unconditionally torn down.  This prevents the
/// next caller from reading a response that was not intended for it (B2).
///
/// The wait for the response is bounded by the [`TAP_CALL_TIMEOUT`] every public
/// operation runs under; a peer that answers too late is a cancellation like any
/// other, and takes the same teardown path, because the unread response would
/// otherwise be handed to the next caller.
async fn send_recv(
    inner: &mut ClientInner,
    req: &crate::types::Request,
) -> Option<crate::types::Response> {
    use crate::framing::{decode_frame, read_frame, write_frame};

    let conn = inner.conn.as_mut()?;

    // Write the request.  If this fails, tear down immediately.
    if write_frame(&mut conn.writer, req).await.is_err() {
        inner.conn = None;
        return None;
    }

    // The request is now on the wire.  Install a drop-guard: if this future is
    // dropped (cancelled) before we finish reading the response, the guard tears
    // down the connection so the stale response cannot be read by the next caller.
    //
    // The guard is disarmed (set to false) only after a successful read.
    let must_teardown = TeardownGuard {
        conn: &mut inner.conn,
    };

    let frame = read_frame(&mut must_teardown.conn.as_mut()?.reader).await;
    let result = frame.ok().and_then(|f| decode_frame(&f).ok());

    // Read completed — disarm the guard.
    must_teardown.disarm();

    if result.is_none() {
        inner.conn = None;
    }
    result
}

/// Drop-guard that tears down a connection if the guarded future is cancelled.
///
/// Constructed while a request is "in flight" (written but not yet answered).
/// If the future is dropped before `disarm()` is called, `Drop` sets the
/// connection to `None`, forcing a reconnect on the next call.
struct TeardownGuard<'a> {
    conn: &'a mut Option<Connection>,
}

impl TeardownGuard<'_> {
    /// Disarm the guard — the response was fully received; keep the connection.
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for TeardownGuard<'_> {
    fn drop(&mut self) {
        *self.conn = None;
    }
}

// ── Test helpers ─────────────────────────────────────────────────────────────

#[cfg(test)]
impl TapClient {
    /// Force the connection closed without touching the handle table.
    /// Used in tests to simulate a clean server restart without relying on
    /// task-abort timing (aborting a `tokio::spawn`'d accept loop does not
    /// immediately close per-connection handler tasks).
    pub async fn disconnect_for_test(&self) {
        let mut inner = self.inner.lock().await;
        inner.conn = None;
    }

    /// Number of entries the shared handle table retains, live or not.
    /// Counting everything is what lets a test show that nothing is kept back.
    pub async fn handle_table_len_for_test(&self) -> usize {
        self.inner.lock().await.handles.len()
    }
}

// ── Reconnect loop (used by tests via tokio::time::pause) ────────────────────

impl TapClient {
    /// Connect with exponential backoff.
    ///
    /// Tries to establish a connection, retrying with increasing delay (250 ms
    /// initial, doubling up to [`BACKOFF_CAP_MS`]).  Tests drive this
    /// deterministically by pausing tokio time and calling
    /// `tokio::time::advance`.
    pub async fn connect_with_backoff(&self) {
        // No socket configured (S4) → nothing to connect to; return immediately
        // rather than spin forever. Taps fail-closed to Unavailable (D3).
        let Some(path) = self.socket_path.as_deref() else {
            return;
        };
        loop {
            let mut inner = self.inner.lock().await;
            if inner.conn.is_some() {
                return;
            }
            if try_connect(&mut inner, path).await.is_some() {
                return;
            }
            let delay = inner.backoff_ms;
            inner.backoff_ms = (delay * 2).min(BACKOFF_CAP_MS);
            drop(inner); // Release lock before sleeping.
            tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
        }
    }
}
