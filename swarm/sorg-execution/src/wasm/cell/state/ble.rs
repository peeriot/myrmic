//! Per-cell in-process BLE backend (Linux / `BlueZ`, via `bluer`).
//!
//! Each cell owns a [`CellBle`]: a lazily-initialized `BlueZ` adapter plus
//! registries of an active scan, connections, and subscriptions. The
//! callback-oriented host functions drive it and return an id/errno
//! synchronously; results are delivered later through a [`BleCallbackSink`],
//! which enqueues a call to the `command_<callback>` handler the cell named.
//!
//! Every operation that streams (scan advertisements, notifications) or blocks
//! (connect, read, write) runs on its own tokio task so the guest host call
//! returns immediately.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bluer::agent::{Agent, AgentHandle, ReqResult, RequestPasskey};
use bluer::gatt::WriteOp;
use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use bluer::{AdapterEvent, AddressType, DeviceEvent, DeviceProperty, Uuid as BluerUuid};
use futures::{Stream, StreamExt};
use myrmic_common::types::ble::{
    Address, Advertisement, Characteristic as WasmChar, ConnectRequest, ConnectionInfo,
    DisconnectReason, DiscoveredDevice, DiscoveryFilter, MAX_ADVERTISED_SERVICE_UUIDS,
    ManufacturerData, NotificationInfo, ReadError, ReadOutcome, ReadRequest, ScanMode, ScanRequest,
    Service as WasmService, ServiceData, SubscribeRequest, Uuid, WriteError, WriteOutcome,
    WriteRequest,
};
use myrmic_common::types::error::{GENERIC_ERROR, SUCCESS};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, warn};

use crate::wasm::cell::state::message_handler::BleCallbackSink;

/// Max manufacturer-data payload carried in a [`ManufacturerData`]
/// (`heapless::Vec<u8, 27>`).
const MFG_PAYLOAD_CAP: usize = 27;

/// Max service-data payload carried in a [`ServiceData`] (`heapless::Vec<u8, 27>`).
const SERVICE_DATA_PAYLOAD_CAP: usize = 27;

/// Per-cell BLE state and operations. Cheaply clonable (shared `Arc` inner).
#[derive(Clone)]
pub(crate) struct CellBle {
    inner: Arc<Mutex<CellBleInner>>,
    sink: BleCallbackSink,
}

struct CellBleInner {
    /// Lazily created on the first operation so non-BLE cells never open an adapter.
    adapter: Option<Adapter>,
    /// The cell's single active scan, if any (a cell scans at most once at a time).
    scan: Option<JoinHandle<()>>,
    connections: HashMap<u32, ConnEntry>,
    subscriptions: HashMap<u32, JoinHandle<()>>,
    next_id: u32,
}

struct ConnEntry {
    device: Arc<Mutex<ConnDevice>>,
    /// Delivered exactly once, by whichever of the disconnect watcher (remote
    /// drop) or `disconnect()` (local, cell-initiated) fires first; the other
    /// finds `None` and stays silent.
    on_disconnected: Arc<Mutex<Option<String>>>,
    /// Watches for a remote disconnect; aborted by `disconnect()` so it can't
    /// also report a disconnect the cell already initiated and knows about.
    watcher: JoinHandle<()>,
}

/// A connected peripheral: the `bluer` device handle plus its discovered
/// characteristics, indexed `service_uuid -> characteristic_uuid`.
struct ConnDevice {
    device: bluer::Device,
    mac_address: Address,
    chars: BTreeMap<Uuid, BTreeMap<Uuid, Characteristic>>,
}

impl CellBleInner {
    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;

        id
    }

    /// Ensures the adapter is initialized, returning an errno on failure.
    async fn ensure_adapter(&mut self) -> Result<&mut Adapter, i32> {
        if self.adapter.is_none() {
            match Adapter::new().await {
                Ok(adapter) => self.adapter = Some(adapter),
                Err(err) => {
                    error!("failed to initialize BLE adapter: {err}");
                    return Err(GENERIC_ERROR);
                }
            }
        }

        Ok(self.adapter.as_mut().expect("adapter just initialized"))
    }
}

impl CellBle {
    pub(crate) fn new(sink: BleCallbackSink) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CellBleInner {
                adapter: None,
                scan: None,
                connections: HashMap::new(),
                subscriptions: HashMap::new(),
                next_id: 0,
            })),
            sink,
        }
    }

    /// Starts the cell's scan (replacing any previous one); returns `SUCCESS` or
    /// a negative errno.
    pub(crate) async fn scan(&self, request: ScanRequest) -> i32 {
        let ScanRequest {
            callback,
            filter,
            mode,
        } = request;
        // `None` filter means "report every advertisement"; the default filter
        // (all fields unset) matches everything.
        let filter = filter.unwrap_or_default();
        // BlueZ has no passive-vs-active scan knob exposed through `bluer`'s safe
        // `DiscoveryFilter` API, and it already merges every field it has ever seen
        // per address (manufacturer data, service data, ...) into the persistent
        // `Device1` object regardless of which packet carried it. So `mode` has no
        // effect here: every scan already behaves like `ScanMode::Active`.
        if mode == ScanMode::Passive {
            warn!(
                "BLE scan requested ScanMode::Passive, which this backend cannot honor \
                 (BlueZ has no passive-scan toggle); scanning as active"
            );
        }

        let mut guard = self.inner.lock().await;
        let adapter = match guard.ensure_adapter().await {
            Ok(adapter) => adapter,
            Err(code) => return code,
        };
        let mut stream = match adapter.discover().await {
            Ok(stream) => stream,
            Err(err) => {
                error!("failed to start scan: {err}");
                return GENERIC_ERROR;
            }
        };

        let sink = self.sink.clone();
        let handle = tokio::spawn(async move {
            while let Some(device) = stream.next().await {
                if !advert_matches(&filter, &device) {
                    continue;
                }
                match postcard::to_allocvec(&device) {
                    Ok(payload) => sink.deliver(callback.clone(), payload).await,
                    Err(err) => error!("failed to serialize discovered device: {err}"),
                }
            }
        });
        // At most one scan per cell: replace (and abort) any prior scan.
        if let Some(previous) = guard.scan.replace(handle) {
            previous.abort();
        }

        SUCCESS
    }

    /// Stops the cell's active scan (idempotent).
    pub(crate) async fn stop_scan(&self) -> i32 {
        if let Some(handle) = self.inner.lock().await.scan.take() {
            handle.abort();
        }

        SUCCESS
    }

    /// Initiates a connection; the outcome is delivered to the request's
    /// callbacks. Returns `SUCCESS` once queued.
    pub(crate) async fn connect(&self, request: ConnectRequest) -> i32 {
        // Fail fast if the adapter cannot be created.
        if let Err(code) = self.inner.lock().await.ensure_adapter().await {
            return code;
        }

        let id = self.inner.lock().await.alloc_id();
        let inner = self.inner.clone();
        let sink = self.sink.clone();
        tokio::spawn(async move { connect_task(inner, sink, id, request).await });

        SUCCESS
    }

    /// Disconnects the connection identified by `id`.
    pub(crate) async fn disconnect(&self, id: u32) -> i32 {
        let entry = self.inner.lock().await.connections.remove(&id);
        match entry {
            Some(entry) => {
                // This is a cell-initiated disconnect: take the slot so the
                // watcher (which will observe the same drop) does not also
                // report it, and abort it since it has no further purpose.
                entry.on_disconnected.lock().await.take();
                entry.watcher.abort();

                let conn = entry.device.lock().await;
                if let Err(err) = conn.device.disconnect().await {
                    error!("disconnect failed: {err}");
                }

                SUCCESS
            }
            None => GENERIC_ERROR,
        }
    }

    /// Subscribes to notifications on a characteristic; returns the subscription
    /// id or a negative errno.
    pub(crate) async fn subscribe(&self, request: SubscribeRequest) -> i32 {
        let SubscribeRequest {
            connection_id,
            characteristic,
            callback,
        } = request;

        let Some(device) = self.device(connection_id).await else {
            error!("subscribe: unknown connection {connection_id}");
            return GENERIC_ERROR;
        };

        let chara = {
            let conn = device.lock().await;
            match find_char(&conn, &characteristic) {
                Some(chara) => chara,
                None => {
                    error!("subscribe: characteristic not found");
                    return GENERIC_ERROR;
                }
            }
        };
        let mut stream = match chara.notify().await {
            Ok(stream) => stream.boxed(),
            Err(err) => {
                error!("failed to subscribe to notifications: {err}");
                return GENERIC_ERROR;
            }
        };

        let sink = self.sink.clone();
        let handle = tokio::spawn(async move {
            while let Some(data) = stream.next().await {
                let notification = NotificationInfo {
                    characteristic,
                    data,
                };
                match postcard::to_allocvec(&notification) {
                    Ok(payload) => sink.deliver(callback.clone(), payload).await,
                    Err(err) => error!("failed to serialize notification: {err}"),
                }
            }
        });

        let id = {
            let mut guard = self.inner.lock().await;
            let id = guard.alloc_id();
            guard.subscriptions.insert(id, handle);
            id
        };

        id.cast_signed()
    }

    /// Unsubscribes the subscription identified by `id`.
    pub(crate) async fn unsubscribe(&self, id: u32) -> i32 {
        match self.inner.lock().await.subscriptions.remove(&id) {
            Some(handle) => {
                handle.abort();
                SUCCESS
            }
            None => GENERIC_ERROR,
        }
    }

    /// Reads a characteristic; the value is delivered to the request's callback.
    pub(crate) async fn read(&self, request: ReadRequest) -> i32 {
        let ReadRequest {
            connection_id,
            characteristic,
            callback,
        } = request;

        let Some(device) = self.device(connection_id).await else {
            error!("read: unknown connection {connection_id}");
            return GENERIC_ERROR;
        };

        let sink = self.sink.clone();
        tokio::spawn(async move {
            let chara = {
                let conn = device.lock().await;
                find_char(&conn, &characteristic)
            };
            let value = match chara {
                Some(chara) => chara.read().await.map_err(|_| ReadError::NotReadable),
                None => Err(ReadError::NotReadable),
            };
            let outcome = ReadOutcome {
                characteristic,
                value,
            };
            match postcard::to_allocvec(&outcome) {
                Ok(payload) => sink.deliver(callback, payload).await,
                Err(err) => error!("failed to serialize read outcome: {err}"),
            }
        });

        SUCCESS
    }

    /// Writes a characteristic. For a write-with-response the result is delivered
    /// to the request's callback; a write-without-response has no callback.
    pub(crate) async fn write(&self, request: WriteRequest) -> i32 {
        let WriteRequest {
            connection_id,
            characteristic,
            data,
            callback,
        } = request;

        let Some(device) = self.device(connection_id).await else {
            error!("write: unknown connection {connection_id}");
            return GENERIC_ERROR;
        };

        let sink = self.sink.clone();
        tokio::spawn(async move {
            let chara = {
                let conn = device.lock().await;
                find_char(&conn, &characteristic)
            };
            let result = match chara {
                Some(chara) => write_char(&chara, &data, callback.is_some())
                    .await
                    .map_err(|_| WriteError::NotWriteable),
                None => Err(WriteError::NotWriteable),
            };

            // Only a write-with-response reports back to the cell.
            if let Some(callback) = callback {
                let outcome = WriteOutcome {
                    characteristic,
                    result,
                };
                match postcard::to_allocvec(&outcome) {
                    Ok(payload) => sink.deliver(callback, payload).await,
                    Err(err) => error!("failed to serialize write outcome: {err}"),
                }
            }
        });

        SUCCESS
    }

    /// Enables pairing with `passkey` on every currently connected device.
    pub(crate) async fn set_pair_passkey(&self, passkey: u32) -> i32 {
        let mut guard = self.inner.lock().await;
        let devices: Vec<_> = guard
            .connections
            .values()
            .map(|entry| entry.device.clone())
            .collect();
        if devices.is_empty() {
            error!("set_pair_passkey: no connected device");
            return GENERIC_ERROR;
        }
        {
            let Some(adapter) = guard.adapter.as_mut() else {
                return GENERIC_ERROR;
            };
            if let Err(err) = adapter.register_passkey_agent(passkey).await {
                error!("set_pair_passkey: agent registration failed: {err}");
                return GENERIC_ERROR;
            }
        }
        drop(guard);

        let mut result = SUCCESS;
        for device in devices {
            let conn = device.lock().await;
            if let Err(err) = conn.device.pair().await {
                error!("set_pair_passkey: pairing failed: {err}");
                result = GENERIC_ERROR;
            }
        }

        result
    }

    async fn device(&self, connection_id: u32) -> Option<Arc<Mutex<ConnDevice>>> {
        self.inner
            .lock()
            .await
            .connections
            .get(&connection_id)
            .map(|entry| entry.device.clone())
    }
}

/// A `BlueZ` adapter with an optional pairing agent, driven directly via `bluer`.
struct Adapter {
    session: bluer::Session,
    adapter: bluer::Adapter,
    security_agent: Option<AgentHandle>,
}

impl Adapter {
    async fn new() -> bluer::Result<Self> {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        if !adapter.is_powered().await? {
            adapter.set_powered(true).await?;
        }

        Ok(Self {
            session,
            adapter,
            security_agent: None,
        })
    }

    /// Starts discovery and returns a stream of [`DiscoveredDevice`]s.
    async fn discover(
        &self,
    ) -> bluer::Result<Pin<Box<dyn Stream<Item = DiscoveredDevice> + Send>>> {
        let adapter = self.adapter.clone();
        // *with_changes so that we can actually see all the properties
        let events = adapter.discover_devices_with_changes().await?;

        Ok(events
            .filter_map(move |event| {
                let adapter = adapter.clone();
                async move {
                    let AdapterEvent::DeviceAdded(addr) = event else {
                        return None;
                    };
                    let device = adapter.device(addr).ok()?;
                    // A failed read of one property must not drop fields that DID read
                    // successfully: BlueZ populates properties incrementally as new
                    // advertisement/scan-response reports arrive, so a transient error on
                    // (e.g.) ServiceData shouldn't discard ManufacturerData read moments
                    // earlier in this same call.
                    let addr_type = device.address_type().await.ok()?;
                    let manufacturer_data = device
                        .manufacturer_data()
                        .await
                        .inspect_err(|err| {
                            warn!("failed to read manufacturer_data for {addr}: {err}");
                        })
                        .ok()
                        .flatten()
                        .and_then(parse_mfg_data);
                    let local_name = device
                        .name()
                        .await
                        .inspect_err(|err| warn!("failed to read name for {addr}: {err}"))
                        .ok()
                        .flatten()
                        .map(|name| truncate_local_name(&name));
                    let service_uuids = device
                        .uuids()
                        .await
                        .inspect_err(|err| warn!("failed to read uuids for {addr}: {err}"))
                        .ok()
                        .flatten()
                        .map(collect_service_uuids)
                        .unwrap_or_default();
                    let service_data = device
                        .service_data()
                        .await
                        .inspect_err(|err| warn!("failed to read service_data for {addr}: {err}"))
                        .ok()
                        .flatten()
                        .and_then(parse_service_data);

                    Some(DiscoveredDevice {
                        address: Address::new(addr.0, addr_type == AddressType::LePublic),
                        advertisement: Advertisement {
                            local_name,
                            manufacturer_data,
                            service_uuids,
                            service_data,
                        },
                    })
                }
            })
            .boxed())
    }

    /// Connects to `address` and discovers its services/characteristics.
    async fn connect(&self, address: Address) -> bluer::Result<ConnDevice> {
        let device = self.adapter.device(bluer::Address(address.octets()))?;

        if !device.is_connected().await? {
            let mut retries = 3;
            loop {
                match device.connect().await {
                    Ok(()) => break,
                    Err(_) if retries > 0 => retries -= 1,
                    Err(err) => return Err(err),
                }
            }
        }

        let mut chars = BTreeMap::new();
        for service in device.services().await? {
            let service_uuid = uuid_convert(service.uuid().await?);
            let mut service_chars = BTreeMap::new();
            for characteristic in service.characteristics().await? {
                let char_uuid = uuid_convert(characteristic.uuid().await?);
                service_chars.insert(char_uuid, characteristic);
            }
            chars.insert(service_uuid, service_chars);
        }

        Ok(ConnDevice {
            device,
            mac_address: address,
            chars,
        })
    }

    /// Registers (or refreshes) a pairing agent that answers passkey requests
    /// with `passkey`.
    async fn register_passkey_agent(&mut self, passkey: u32) -> bluer::Result<()> {
        static PASSKEY: AtomicU32 = AtomicU32::new(0);

        // `bluer`'s `Agent::request_passkey` field wants a callback returning a
        // future, so this must stay `async` even though it never awaits.
        #[allow(clippy::unused_async)]
        async fn request_passkey(_req: RequestPasskey) -> ReqResult<u32> {
            Ok(PASSKEY.load(Ordering::Relaxed))
        }

        PASSKEY.store(passkey, Ordering::Relaxed);

        let agent = Agent {
            request_default: true,
            request_passkey: Some(Box::new(|req| Box::pin(request_passkey(req)))),
            ..Default::default()
        };
        self.security_agent = Some(self.session.register_agent(agent).await?);

        Ok(())
    }
}

async fn connect_task(
    inner: Arc<Mutex<CellBleInner>>,
    sink: BleCallbackSink,
    id: u32,
    request: ConnectRequest,
) {
    let ConnectRequest {
        address,
        on_connected,
        on_disconnected,
    } = request;

    let connect_result = {
        let mut guard = inner.lock().await;
        let Ok(adapter) = guard.ensure_adapter().await else {
            drop(guard);
            deliver_reason(&sink, on_disconnected, DisconnectReason::ConnectionFailed).await;
            return;
        };
        adapter.connect(address).await
    };

    match connect_result {
        Ok(conn) => {
            let info = build_connection_info(id, &conn);
            let device = conn.device.clone();
            let on_disconnected = Arc::new(Mutex::new(Some(on_disconnected)));
            let watcher = tokio::spawn(watch_disconnect(
                inner.clone(),
                sink.clone(),
                id,
                device,
                on_disconnected.clone(),
            ));
            inner.lock().await.connections.insert(
                id,
                ConnEntry {
                    device: Arc::new(Mutex::new(conn)),
                    on_disconnected,
                    watcher,
                },
            );
            match postcard::to_allocvec(&info) {
                Ok(payload) => sink.deliver(on_connected, payload).await,
                Err(err) => error!("failed to serialize connection info: {err}"),
            }
        }
        Err(err) => {
            error!("failed to connect: {err}");
            deliver_reason(&sink, on_disconnected, DisconnectReason::ConnectionFailed).await;
        }
    }
}

/// Performs a GATT write, with or without a response.
async fn write_char(chara: &Characteristic, data: &[u8], with_response: bool) -> bluer::Result<()> {
    if with_response {
        let request = CharacteristicWriteRequest {
            op_type: WriteOp::Request,
            ..Default::default()
        };
        chara.write_ext(data, &request).await
    } else {
        chara.write(data).await
    }
}

/// Watches a connected device for a remote disconnect and reports it so that the cell knows if the
/// host disconnected other than a direct cell `disconnect()`.
async fn watch_disconnect(
    inner: Arc<Mutex<CellBleInner>>,
    sink: BleCallbackSink,
    id: u32,
    device: bluer::Device,
    on_disconnected: Arc<Mutex<Option<String>>>,
) {
    let Ok(mut events) = device.events().await else {
        return;
    };
    while let Some(event) = events.next().await {
        if matches!(
            event,
            DeviceEvent::PropertyChanged(DeviceProperty::Connected(false))
        ) {
            break;
        }
    }

    inner.lock().await.connections.remove(&id);
    if let Some(callback) = on_disconnected.lock().await.take() {
        deliver_reason(&sink, callback, DisconnectReason::RemoteClosed).await;
    }
}

async fn deliver_reason(sink: &BleCallbackSink, callback: String, reason: DisconnectReason) {
    match postcard::to_allocvec(&reason) {
        Ok(payload) => sink.deliver(callback, payload).await,
        Err(err) => error!("failed to serialize disconnect reason: {err}"),
    }
}

/// Builds the wire [`ConnectionInfo`] from a connected device by mapping its GATT
/// map into the wire's UUID-keyed maps.
fn build_connection_info(id: u32, conn: &ConnDevice) -> ConnectionInfo {
    let gatt_services = conn
        .chars
        .iter()
        .map(|(service_uuid, characteristics)| {
            let characteristics = characteristics
                .keys()
                .map(|char_uuid| {
                    (
                        *char_uuid,
                        WasmChar {
                            uuid: *char_uuid,
                            service_uuid: *service_uuid,
                        },
                    )
                })
                .collect();

            (*service_uuid, WasmService { characteristics })
        })
        .collect();

    ConnectionInfo {
        id,
        mac_address: conn.mac_address,
        gatt_services,
    }
}

/// Finds (and clones) the `bluer` characteristic for a wire characteristic, so
/// the GATT operation can run without holding the connection lock.
fn find_char(conn: &ConnDevice, characteristic: &WasmChar) -> Option<Characteristic> {
    conn.chars
        .get(&characteristic.service_uuid)?
        .get(&characteristic.uuid)
        .cloned()
}

/// Extracts the lowest-UUID manufacturer-specific entry into a [`ManufacturerData`], truncating the
/// payload to what the wire type can hold.
fn parse_mfg_data(data: HashMap<u16, Vec<u8>>) -> Option<ManufacturerData> {
    // Use lowest UUID rather than first because bluer randomizes the order by using HashMaps
    let (company_identifier, mut payload) = data.into_iter().min_by_key(|(id, _)| *id)?;
    payload.truncate(MFG_PAYLOAD_CAP);

    Some(ManufacturerData {
        company_identifier,
        payload: heapless::Vec::from_slice(&payload).unwrap_or_default(),
    })
}

/// Extracts the lowest-UUID service-data entry into a [`ServiceData`], truncating the payload to
/// what the wire type can hold. See [`parse_mfg_data`] for why the entry must be picked
/// deterministically rather than via `.next()`.
fn parse_service_data(data: HashMap<BluerUuid, Vec<u8>>) -> Option<ServiceData> {
    let (uuid, mut payload) = data.into_iter().min_by_key(|(uuid, _)| *uuid)?;
    payload.truncate(SERVICE_DATA_PAYLOAD_CAP);

    Some(ServiceData {
        uuid: uuid_convert(uuid),
        payload: heapless::Vec::from_slice(&payload).unwrap_or_default(),
    })
}

/// Truncates an advertised local name to the wire type's capacity, at a char
/// boundary.
fn truncate_local_name(name: &str) -> heapless::String<32> {
    let mut out = heapless::String::new();
    for ch in name.chars() {
        if out.push(ch).is_err() {
            break;
        }
    }

    out
}

/// Converts the advertised service UUID set into the wire list, capped at
/// [`MAX_ADVERTISED_SERVICE_UUIDS`].
fn collect_service_uuids(
    uuids: HashSet<BluerUuid>,
) -> heapless::Vec<Uuid, MAX_ADVERTISED_SERVICE_UUIDS> {
    let mut out = heapless::Vec::new();
    for uuid in uuids {
        if out.push(uuid_convert(uuid)).is_err() {
            break;
        }
    }

    out
}

/// Whether a scanned advertisement matches the filter. Every constraint the
/// filter sets (`company_id`, `local_name`, `service_uuid`) is evaluated against
/// the advertisement carried on the [`DiscoveredDevice`]; an unset field passes.
fn advert_matches(filter: &DiscoveryFilter, device: &DiscoveredDevice) -> bool {
    let advertisement = &device.advertisement;

    if let Some(company_id) = filter.company_id {
        match &advertisement.manufacturer_data {
            Some(data) if data.company_identifier == company_id => {}
            _ => return false,
        }
    }
    if let Some(local_name) = &filter.local_name {
        match &advertisement.local_name {
            Some(name) if name == local_name => {}
            _ => return false,
        }
    }
    if let Some(service_uuid) = filter.service_uuid {
        let in_uuid_list = advertisement.service_uuids.contains(&service_uuid);
        let in_service_data = advertisement
            .service_data
            .as_ref()
            .is_some_and(|data| data.uuid == service_uuid);
        if !in_uuid_list && !in_service_data {
            return false;
        }
    }

    true
}

/// Converts a `bluer` UUID into the wire [`Uuid`], recognizing the 16-bit
/// Bluetooth base range.
fn uuid_convert(uuid: BluerUuid) -> Uuid {
    // 16-bit UUIDs follow the pattern 0000xxxx-0000-1000-8000-00805f9b34fb.
    let hyphenated = uuid.hyphenated().to_string();
    let (start, rest) = hyphenated.split_at(4);
    let (num, end) = rest.split_at(4);
    if start == "0000" && end == "-0000-1000-8000-00805f9b34fb" {
        Uuid::Bit16(u16::from_str_radix(num, 16).unwrap_or_default())
    } else {
        Uuid::Bit128(<[u8; 16]>::try_from(uuid.as_bytes().as_slice()).expect("UUID is 16 bytes"))
    }
}
