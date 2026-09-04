//! Persistent BLE manager task.
//!
//! Owns the radio ([`BleContext`]) and all streaming state (active scan,
//! connection, notification subscriptions). It runs concurrently with the
//! request pipeline: the callback-oriented host functions forward a
//! [`BleCommand`] via [`forward`] (routed through the request pipeline's
//! `execute_request`), the task performs the operation reusing the proven ops in
//! [`super::ble`], and delivers results by pushing a
//! [`CellMessage::BleCallback`](crate::CellMessage::BleCallback) with
//! [`enqueue_ble_callback`].
//!
//! Because the task, not the request pipeline, owns the long-lived advertisement
//! and notification streams, a `forward` returns quickly (an id or a queued
//! `SUCCESS`) and the pipeline stays free for the callbacks to flow.

use alloc::string::String;
use alloc::vec::Vec;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, with_timeout};
use myrmic_common::types::ble::{
    Address, Characteristic as WasmChar, DisconnectReason, DiscoveredDevice, DiscoveryFilter,
    NotificationInfo, ReadError, ReadOutcome, ScanMode, WriteError, WriteOutcome,
};

use crate::async_request::Error;
use crate::async_request::ble::{self, BleContext, NotifOutcome};
use crate::async_request::cell_host::{enqueue_ble_callback, enqueue_ble_callback_or_wait};

/// Maximum concurrent notification subscriptions per connection.
const MAX_SUBS: usize = 8;

/// A command sent to the BLE manager task.
pub(crate) enum BleCommand {
    /// Starts the scan
    Scan {
        /// Advertisment reporting callback
        callback: String,
        /// Discovery Filter (Default = No Filter)
        filter: DiscoveryFilter,
        /// Whether to also receive scan responses
        mode: ScanMode,
    },
    /// Stops the scan (some ongoing advertisement reports can still reach the cell)
    StopScan,
    /// Connects to a device
    Connect {
        address: Address,
        on_connected: String,
        on_disconnected: String,
    },
    /// Disconnects from the device
    Disconnect { id: u32 },
    /// Subscribes to a characteristic
    Subscribe {
        connection_id: u32,
        characteristic: WasmChar,
        callback: String,
    },
    /// Unsubscribes from a subscribed characteristic
    Unsubscribe { id: u32 },
    /// Reads a characteristic
    Read {
        connection_id: u32,
        characteristic: WasmChar,
        callback: String,
    },
    /// Writes to a characteristic
    Write {
        connection_id: u32,
        characteristic: WasmChar,
        data: Vec<u8>,
        callback: Option<String>,
    },
    /// Sets a Pairing Passkey
    SetPairPasskey { passkey: u32 },
    /// Wipes all state back to idle (see [`reset_to_idle`]). Not guest-facing:
    /// issued by the runtime itself when the owning cell is torn down.
    Reset,
}

// WAMR host requests are synchronous, so only one command is ever in flight; 2 gives a small
// buffer.
static BLE_COMMANDS: Channel<CriticalSectionRawMutex, BleCommand, 2> = Channel::new();

/// Reply channel for a command. Single-in-flight (WAMR is synchronous), so a
/// shared `Signal` is safe — every reply has a matching awaiter.
static BLE_RESPONSE: Signal<CriticalSectionRawMutex, Result<u32, Error>> = Signal::new();

/// Sends a command to the BLE manager task and waits for its response.
pub(crate) async fn forward(cmd: BleCommand) -> Result<u32, Error> {
    BLE_RESPONSE.reset();
    BLE_COMMANDS.send(cmd).await;

    BLE_RESPONSE.wait().await
}

struct ScanEntry {
    callback: String,
    filter: DiscoveryFilter,
    mode: ScanMode,
}

struct SubEntry {
    id: u32,
    /// ATT handle used to dispatch incoming notifications to this subscription.
    handle: u16,
    characteristic: WasmChar,
    callback: String,
}

struct State {
    ctx: BleContext,
    scan: Option<ScanEntry>,
    conn_id: Option<u32>,
    on_disconnected: Option<String>,
    subs: heapless::Vec<SubEntry, MAX_SUBS>,
    next_id: u32,
}

/// Which stream the task should currently wait on, alongside the command channel.
enum Mode {
    Scanning(DiscoveryFilter, ScanMode),
    Notifications,
    Idle,
}

impl State {
    fn new() -> Self {
        Self {
            ctx: BleContext::default(),
            scan: None,
            conn_id: None,
            on_disconnected: None,
            subs: heapless::Vec::new(),
            next_id: 0,
        }
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        id
    }

    fn mode(&self) -> Mode {
        if let Some(scan) = &self.scan {
            Mode::Scanning(scan.filter.clone(), scan.mode)
        } else if ble::has_notif_sub(&self.ctx) {
            Mode::Notifications
        } else {
            Mode::Idle
        }
    }
}

/// The BLE manager task. Owns the radio and all streaming state, and runs
/// concurrently with the request pipeline. Commands are sent via `forward`,
/// results are delivered via `enqueue_ble_callback`.
#[embassy_executor::task]
pub async fn ble_manager_task() {
    let mut state = State::new();

    loop {
        match state.mode() {
            Mode::Scanning(filter, _mode) => {
                match select(
                    BLE_COMMANDS.receive(),
                    ble::next_matching_advert(&mut state.ctx, &filter),
                )
                .await
                {
                    Either::First(cmd) => handle_command(&mut state, cmd).await,
                    Either::Second(device) => deliver_advert(&state, &device),
                }
            }
            Mode::Notifications => {
                match select(
                    BLE_COMMANDS.receive(),
                    ble::wait_notification(&mut state.ctx),
                )
                .await
                {
                    Either::First(cmd) => handle_command(&mut state, cmd).await,
                    Either::Second(NotifOutcome::Message { handle, payload }) => {
                        deliver_notification(&state, handle, payload);
                    }
                    Either::Second(NotifOutcome::Disconnected) => {
                        handle_remote_disconnect(&mut state).await;
                    }
                }
            }
            Mode::Idle => {
                if state.conn_id.is_some() {
                    match select(BLE_COMMANDS.receive(), ble::wait_disconnect(&mut state.ctx)).await
                    {
                        Either::First(cmd) => handle_command(&mut state, cmd).await,
                        Either::Second(()) => handle_remote_disconnect(&mut state).await,
                    }
                } else {
                    let cmd = BLE_COMMANDS.receive().await;
                    handle_command(&mut state, cmd).await;
                }
            }
        }
    }
}

async fn handle_command(state: &mut State, cmd: BleCommand) {
    match cmd {
        // Begin scanning and confirm immediately (callbacks will arrive asynchronously).
        BleCommand::Scan {
            filter,
            callback,
            mode,
        } => start_scan(state, filter, callback, mode).await,
        BleCommand::StopScan => {
            ble::stop_scanning(&mut state.ctx);
            state.scan = None;
            BLE_RESPONSE.signal(Ok(0));
        }
        BleCommand::Connect {
            address,
            on_connected,
            on_disconnected,
        } => connect(state, address, on_connected, on_disconnected).await,
        BleCommand::Disconnect { id } => {
            if state.conn_id == Some(id) {
                ble::disconnect_active(&mut state.ctx).await;
                clear_connection(state);
                BLE_RESPONSE.signal(Ok(0));
            } else {
                BLE_RESPONSE.signal(Err(Error::Generic));
            }
        }
        BleCommand::Subscribe {
            connection_id,
            characteristic,
            callback,
        } => subscribe(state, connection_id, characteristic, callback).await,
        BleCommand::Unsubscribe { id } => {
            if let Some(pos) = state.subs.iter().position(|sub| sub.id == id) {
                let entry = state.subs.swap_remove(pos);
                ble::char_unregister(&mut state.ctx, entry.characteristic).await;
                // The notification subscriber is shared across every characteristic;
                // only drop it once the last subscription is gone.
                if state.subs.is_empty() {
                    ble::clear_notif_sub(&mut state.ctx);
                }
                BLE_RESPONSE.signal(Ok(0));
            } else {
                BLE_RESPONSE.signal(Err(Error::Generic));
            }
        }
        BleCommand::Read {
            connection_id,
            characteristic,
            callback,
        } => {
            if state.conn_id != Some(connection_id) {
                BLE_RESPONSE.signal(Err(Error::Generic));
                return;
            }
            BLE_RESPONSE.signal(Ok(0));
            let value = ble::char_read(&mut state.ctx, characteristic).await;
            if matches!(value, Err(Error::Disconnected)) {
                handle_remote_disconnect(state).await;
            }
            deliver_read_outcome(characteristic, value, callback).await;
        }
        BleCommand::Write {
            connection_id,
            characteristic,
            data,
            callback,
        } => {
            if state.conn_id != Some(connection_id) {
                BLE_RESPONSE.signal(Err(Error::Generic));
                return;
            }
            BLE_RESPONSE.signal(Ok(0));
            let with_response = callback.is_some();
            let result = ble::char_write(&mut state.ctx, characteristic, data, with_response).await;
            if matches!(result, Err(Error::Disconnected)) {
                handle_remote_disconnect(state).await;
            }
            if let Some(callback) = callback {
                deliver_write_outcome(characteristic, result, callback).await;
            }
        }
        BleCommand::SetPairPasskey { passkey } => match ble::set_passkey(&state.ctx, passkey).await
        {
            Ok(()) => BLE_RESPONSE.signal(Ok(0)),
            Err(_) => BLE_RESPONSE.signal(Err(Error::Generic)),
        },
        BleCommand::Reset => {
            reset_to_idle(state).await;
            BLE_RESPONSE.signal(Ok(0));
        }
    }
}

/// Connects to the address (and handles `on_connected`/`on_disconnected`) callbacks
async fn connect(
    state: &mut State,
    address: Address,
    on_connected: String,
    on_disconnected: String,
) {
    // Single-connection model: reject a new connect while one is already
    // live. A cell must `disconnect` before connecting again.
    if state.conn_id.is_some() {
        BLE_RESPONSE.signal(Err(Error::Generic));
        return;
    }
    // Radio exclusion: stop scanning before connecting.
    state.scan = None;
    ble::stop_scanning(&mut state.ctx);
    let id = state.alloc_id();
    // The attempt is queued; success/failure arrives on the callbacks.
    BLE_RESPONSE.signal(Ok(0));
    match ble::connect_build_info(&mut state.ctx, address, id).await {
        Ok(payload) => {
            // Only commit the connection once the cell has actually been told
            // about it - otherwise the host would believe it holds a link the
            // guest has never heard of and can neither use nor drop.
            if enqueue_ble_callback(on_connected, payload) {
                state.conn_id = Some(id);
                state.on_disconnected = Some(on_disconnected);
            } else {
                log::error!("[ble] on_connected dropped (cell channel full); tearing down");
                ble::disconnect_active(&mut state.ctx).await;
                deliver_reason(on_disconnected, DisconnectReason::ConnectionFailed).await;
            }
        }
        Err(_) => deliver_reason(on_disconnected, DisconnectReason::ConnectionFailed).await,
    }
}

/// Starts a scan
async fn start_scan(state: &mut State, filter: DiscoveryFilter, callback: String, mode: ScanMode) {
    if state.conn_id.is_some() {
        // Radio exclusion: an active connection must be dropped before scanning.
        // Tear it down here so that the cell is told.
        ble::disconnect_active(&mut state.ctx).await;
        notify_disconnect(state, DisconnectReason::LocalClosed).await;
    }
    match ble::ensure_scanning(&mut state.ctx, mode).await {
        Ok(()) => {
            // A cell has at most one active scan; replace any previous one.
            state.scan = Some(ScanEntry {
                callback,
                filter,
                mode,
            });
            BLE_RESPONSE.signal(Ok(0));
        }
        Err(e) => BLE_RESPONSE.signal(Err(e)),
    }
}

async fn subscribe(
    state: &mut State,
    connection_id: u32,
    characteristic: WasmChar,
    callback: String,
) {
    if state.conn_id != Some(connection_id) {
        BLE_RESPONSE.signal(Err(Error::Generic));
        return;
    }
    let Some(handle) = ble::char_handle(&state.ctx, &characteristic) else {
        BLE_RESPONSE.signal(Err(Error::Generic));
        return;
    };
    if let Err(e) = ble::char_register_notif(&mut state.ctx, characteristic).await {
        if matches!(e, Error::Disconnected) {
            handle_remote_disconnect(state).await;
        }
        BLE_RESPONSE.signal(Err(Error::Generic));
        return;
    }
    if ble::ensure_notif_sub(&mut state.ctx).is_err() {
        BLE_RESPONSE.signal(Err(Error::Generic));
        return;
    }

    let id = state.alloc_id();
    let entry = SubEntry {
        id,
        handle,
        characteristic,
        callback,
    };
    if state.subs.push(entry).is_err() {
        BLE_RESPONSE.signal(Err(Error::Generic));
        return;
    }

    BLE_RESPONSE.signal(Ok(id));
}

/// Bounds how long teardown waits for an in-flight disconnect handshake - the next cell's view of the radio must be
/// deterministic even if a peripheral is unresponsive.
const RESET_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Wipes every per-scan/per-connection field back to what a freshly-booted `ble_manager_task` starts with, so the next
/// cell sees the same idle radio the very first cell would have.
async fn reset_to_idle(state: &mut State) {
    state.scan = None;
    ble::stop_scanning(&mut state.ctx);

    if with_timeout(
        RESET_DISCONNECT_TIMEOUT,
        ble::disconnect_active(&mut state.ctx),
    )
    .await
    .is_err()
    {
        log::warn!("[ble] disconnect during reset timed out; forcing state idle anyway");
    }
    // Unconditional: a timed-out disconnect_active above may have been
    // cancelled mid-await, before its own internal reset_connection ran.
    ble::reset_connection(&mut state.ctx);

    clear_connection(state);
    state.next_id = 0;
}

fn clear_connection(state: &mut State) {
    state.conn_id = None;
    state.on_disconnected = None;
    state.subs.clear();
}

/// Delivers `on_disconnected` (if still armed) and clears the connection.
async fn notify_disconnect(state: &mut State, reason: DisconnectReason) {
    if let Some(callback) = state.on_disconnected.take() {
        deliver_reason(callback, reason).await;
    }
    clear_connection(state);
}

async fn handle_remote_disconnect(state: &mut State) {
    ble::reset_connection(&mut state.ctx);
    notify_disconnect(state, DisconnectReason::RemoteClosed).await;
}

/// Guaranteed delivery: a lost disconnect reason leaves the cell believing a
/// dead link is still up (swarm#1306).
async fn deliver_reason(callback: String, reason: DisconnectReason) {
    match postcard::to_allocvec(&reason) {
        Ok(bytes) => {
            enqueue_ble_callback_or_wait("disconnect reason", callback, bytes).await;
        }
        Err(_) => log::error!("[ble] failed to serialize disconnect reason"),
    }
}

/// Lossy by design: advertisements repeat every couple of seconds, so one may
/// be shed under backpressure — but never silently (swarm#1306).
fn deliver_advert(state: &State, device: &DiscoveredDevice) {
    let Some(scan) = &state.scan else { return };
    match postcard::to_allocvec(device) {
        Ok(payload) => {
            if !enqueue_ble_callback(scan.callback.clone(), payload) {
                log::debug!(
                    "[ble] advert for '{}' dropped (cell channel full)",
                    scan.callback
                );
            }
        }
        Err(_) => log::error!("[ble] failed to serialize discovered device"),
    }
}

/// Lossy under backpressure: shedding beats stalling the radio loop during a
/// notification burst, but a drop is real subscribed data lost, so it is
/// logged at warn (swarm#1306).
fn deliver_notification(state: &State, handle: u16, payload: Vec<u8>) {
    let Some(sub) = state.subs.iter().find(|sub| sub.handle == handle) else {
        // Notification for a characteristic we are not subscribed to.
        return;
    };
    let notification = NotificationInfo {
        characteristic: sub.characteristic,
        data: payload,
    };
    match postcard::to_allocvec(&notification) {
        Ok(bytes) => {
            if !enqueue_ble_callback(sub.callback.clone(), bytes) {
                log::warn!(
                    "[ble] notification for '{}' dropped (cell channel full)",
                    sub.callback
                );
            }
        }
        Err(_) => log::error!("[ble] failed to serialize notification"),
    }
}

/// Guaranteed delivery: the cell is awaiting this outcome and a drop would
/// leave it waiting forever, with no timeout and no error (swarm#1306).
async fn deliver_read_outcome(
    characteristic: WasmChar,
    value: Result<Vec<u8>, Error>,
    callback: String,
) {
    let value = value.map_err(|err| match err {
        Error::RequiresSecurity => ReadError::RequiresSecurity,
        _ => ReadError::NotReadable,
    });
    let outcome = ReadOutcome {
        characteristic,
        value,
    };
    match postcard::to_allocvec(&outcome) {
        Ok(bytes) => {
            enqueue_ble_callback_or_wait("read outcome", callback, bytes).await;
        }
        Err(_) => log::error!("[ble] failed to serialize read outcome"),
    }
}

/// Guaranteed delivery — see [`deliver_read_outcome`] (swarm#1306).
async fn deliver_write_outcome(
    characteristic: WasmChar,
    result: Result<(), Error>,
    callback: String,
) {
    let result = result.map_err(|err| match err {
        Error::RequiresSecurity => WriteError::RequiresSecurity,
        _ => WriteError::NotWriteable,
    });
    let outcome = WriteOutcome {
        characteristic,
        result,
    };
    match postcard::to_allocvec(&outcome) {
        Ok(bytes) => {
            enqueue_ble_callback_or_wait("write outcome", callback, bytes).await;
        }
        Err(_) => log::error!("[ble] failed to serialize write outcome"),
    }
}
