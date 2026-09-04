//! BLE async request

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::pubsub::{Subscriber, WaitResult};
use embassy_time::{Duration, with_timeout};
use esp_nimble_host::Scanner;
use esp_nimble_host::characteristic::Characteristic;
use esp_nimble_host::data::{BleAddr, RawAdvertisement, uuid_from_u16};
use esp_nimble_host::error::{ConnectError, GattError, NimbleError};
use esp_nimble_host::peripheral::{
    EventSubscriber, NotificationSubscriber, Peripheral, PeripheralEvent,
};
use esp_nimble_host::uuid::Uuid;
use myrmic_common::types::ble::{
    Address, Advertisement, Characteristic as WasmChar, ConnectionInfo, DiscoveredDevice,
    DiscoveryFilter, MAX_ADVERTISED_SERVICE_UUIDS, ManufacturerData, ScanMode,
    Service as WasmService, ServiceData, Uuid as BleUuid,
};
use wasm_runtime_macros::requests;

use crate::async_request::{Error, Request, Response, ResponseResult};

// Advertisement PubSubChannel constants matching esp-nimble-host's Scanner
// (ADV_PUBSUB_CAP = 32, subs = 4, pubs = 1). The notification/event subscribers
// are owning types from esp-nimble-host and need no lifetime juggling here.
type AdvSub = Subscriber<'static, CriticalSectionRawMutex, RawAdvertisement, 32, 4, 1>;

/// Bounds how long a scan request waits for the BLE controller to sync before
/// giving up. A controller that never syncs (radio init failure, wedged HCI
/// transport) must not block the shared request pipeline forever.
const SYNC_TIMEOUT: Duration = Duration::from_secs(10);

struct CharEntry {
    svc_uuid: Uuid,
    characteristic: Characteristic,
}

/// Radio-exclusion states. Scanning and an active connection cannot coexist.
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) enum RadioState {
    #[default]
    Idle,
    Scanning,
    Connected,
}

#[derive(Default)]
pub(crate) struct BleContext {
    pub(crate) radio: RadioState,
    scanner: Option<&'static mut Scanner<CriticalSectionRawMutex>>,
    sub: Option<AdvSub>,
    synced: bool,
    peripheral: Option<Peripheral<CriticalSectionRawMutex>>,
    char_cache: Vec<CharEntry>,
    notif_sub: Option<NotificationSubscriber<CriticalSectionRawMutex>>,
    events_sub: Option<EventSubscriber<CriticalSectionRawMutex>>,
    adv_cache: heapless::Vec<AdvCacheEntry, ADV_CACHE_CAP>,
}

impl core::fmt::Debug for BleContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BleContext")
            .field("radio", &self.radio)
            .finish_non_exhaustive()
    }
}

requests! {
    wrap(BleRequest => Request::Ble),
    unwrap(Response::Ble => BleResponse);

    Scan { callback: String, filter: DiscoveryFilter, mode: ScanMode }                => ResponseResult,
    StopScan                                                                         => ResponseResult,
    Connect { address: Address, on_connected: String, on_disconnected: String }      => ResponseResult,
    Disconnect { id: u32 }                                                           => ResponseResult,
    CharSubscribe { connection_id: u32, characteristic: WasmChar, callback: String } => Result<u32, Error>,
    CharUnsubscribe { id: u32 }                                                      => ResponseResult,
    CharRead { connection_id: u32, characteristic: WasmChar, callback: String }      => ResponseResult,
    CharWrite {
        connection_id: u32,
        characteristic: WasmChar,
        data: Vec<u8>,
        callback: Option<String>,
    } => ResponseResult,
    SetPairPasskey { passkey: u32 }                                                  => ResponseResult,
    Reset                                                                            => ResponseResult,
}

/// Forwards a BLE request to the persistent [`ble_task`](crate::async_request::ble_task)
/// manager and returns the response.
pub(crate) async fn execute_request(req: BleRequest) -> BleResponse {
    use crate::async_request::ble_task::{BleCommand, forward};

    match req {
        BleRequest::Scan {
            filter,
            callback,
            mode,
        } => BleResponse::Scan(
            forward(BleCommand::Scan {
                filter,
                callback,
                mode,
            })
            .await
            .map(|_| ()),
        ),
        BleRequest::StopScan => {
            BleResponse::StopScan(forward(BleCommand::StopScan).await.map(|_| ()))
        }
        BleRequest::Connect {
            address,
            on_connected,
            on_disconnected,
        } => BleResponse::Connect(
            forward(BleCommand::Connect {
                address,
                on_connected,
                on_disconnected,
            })
            .await
            .map(|_| ()),
        ),
        BleRequest::Disconnect { id } => {
            BleResponse::Disconnect(forward(BleCommand::Disconnect { id }).await.map(|_| ()))
        }
        BleRequest::CharSubscribe {
            connection_id,
            characteristic,
            callback,
        } => BleResponse::CharSubscribe(
            forward(BleCommand::Subscribe {
                connection_id,
                characteristic,
                callback,
            })
            .await,
        ),
        BleRequest::CharUnsubscribe { id } => {
            BleResponse::CharUnsubscribe(forward(BleCommand::Unsubscribe { id }).await.map(|_| ()))
        }
        BleRequest::CharRead {
            connection_id,
            characteristic,
            callback,
        } => BleResponse::CharRead(
            forward(BleCommand::Read {
                connection_id,
                characteristic,
                callback,
            })
            .await
            .map(|_| ()),
        ),
        BleRequest::CharWrite {
            connection_id,
            characteristic,
            data,
            callback,
        } => BleResponse::CharWrite(
            forward(BleCommand::Write {
                connection_id,
                characteristic,
                data,
                callback,
            })
            .await
            .map(|_| ()),
        ),
        BleRequest::SetPairPasskey { passkey } => BleResponse::SetPairPasskey(
            forward(BleCommand::SetPairPasskey { passkey })
                .await
                .map(|_| ()),
        ),
        BleRequest::Reset => BleResponse::Reset(forward(BleCommand::Reset).await.map(|_| ())),
    }
}

/// Ensure the radio is scanning with a live advertisement subscription, leaving
/// the subscriber in `ctx.sub`. Shared setup for the scan path.
///
/// Assumes the caller has already torn down any active connection (radio
/// exclusion): `ble_task` owns that decision because it must also reconcile
/// its own connection bookkeeping and notify the cell of the disconnect.
pub(crate) async fn ensure_scanning(ctx: &mut BleContext, mode: ScanMode) -> Result<(), Error> {
    if !ctx.synced {
        with_timeout(SYNC_TIMEOUT, esp_nimble_host::wait_for_sync())
            .await
            .map_err(|_| Error::Timeout)?;
        ctx.synced = true;
    }

    if ctx.scanner.is_none() {
        let scanner: &'static mut Scanner<CriticalSectionRawMutex> =
            Box::leak(Box::new(Scanner::new()));
        ctx.scanner = Some(scanner);
    }

    // (Re)start scanning on every call. start_scan() always cancels any prior scan
    // first, recovering from stale state after a NimBLE-internal
    // BLE_GAP_EVENT_DISC_COMPLETE.
    //
    // 60 ms interval / 30 ms window: the GAP-recommended fast-scan operating point
    // (`BLE_GAP_SCAN_FAST_INTERVAL_MAX` / `BLE_GAP_SCAN_FAST_WINDOW`), in units of
    // 0.625 ms. BLE and WiFi share one radio, and scanning at an 80% duty cycle
    // starves WiFi badly enough to fail zenoh publishes; measured on an ESP32-C5,
    // 18 publish failures in 10 minutes at 80% against none at 50%. Detection
    // probability is window/interval, so halving the duty cycle halves the
    // advertisement rate and nothing more.
    let params = esp_nimble_host::data::BleGapDiscParams {
        itvl: 96,
        window: 48,
        passive: mode == ScanMode::Passive,
        ..Default::default()
    };
    ctx.scanner
        .as_mut()
        .unwrap()
        .start_scan(Some(params))
        .map_err(|_| Error::Generic)?;
    ctx.radio = RadioState::Scanning;

    if ctx.sub.is_none() {
        let scanner: &Scanner<CriticalSectionRawMutex> = ctx.scanner.as_ref().unwrap();
        // SAFETY: the `Scanner` is `Box::leak`'d above, so the `PubSubChannel` it owns
        // lives for 'static and the `Subscriber` borrows only that channel. We extend
        // the lifetime because `Scanner::subscribe` ties its result to `&self`
        // artificially. Proper fix is upstream — make `subscribe` return
        // `Subscriber<'static>` — then this transmute can be deleted.
        let sub: AdvSub = unsafe {
            core::mem::transmute::<
                Subscriber<'_, CriticalSectionRawMutex, RawAdvertisement, 32, 4, 1>,
                AdvSub,
            >(scanner.subscribe().map_err(|_| Error::Generic)?)
        };
        ctx.sub = Some(sub);
    }

    Ok(())
}

/// Manufacturer-specific advertising data starts with a 2-byte little-endian
/// company identifier, followed by the payload.
const MFG_COMPANY_ID_LEN: usize = 2;
/// Capacity of `myrmic_common::types::ble::ManufacturerData::payload` (`heapless::Vec<u8, 27>`).
/// The payload slice is truncated to this so `Vec::from_slice` cannot overflow;
/// keep in sync with `ManufacturerData`.
const MFG_PAYLOAD_CAP: usize = 27;

/// Capacity of `myrmic_common::types::ble::ServiceData::payload` (`heapless::Vec<u8, 27>`). Keep
/// in sync with `ServiceData`.
const SERVICE_DATA_PAYLOAD_CAP: usize = 27;

fn raw_to_discovered(raw: &RawAdvertisement) -> DiscoveredDevice {
    // Parse the raw BLE AD structure directly instead of relying on
    // ble_hs_adv_parse_fields, which rejects some valid non-standard formats
    // (e.g. Govee sensors).  AD type 0xFF = manufacturer-specific data.
    let advertisement = Advertisement {
        local_name: parse_local_name_from_ad(raw.data()),
        manufacturer_data: parse_mfg_from_ad(raw.data()),
        service_uuids: parse_service_uuids_from_ad(raw.data()),
        service_data: parse_service_data_from_ad(raw.data()),
    };
    let address = ble_addr_to_address(raw.addr());
    DiscoveredDevice {
        address,
        advertisement,
    }
}

/// Iterate the BLE AD (Advertising Data) structure, yielding `(ad_type, value)`
/// for each length/type/value element. A value claiming to extend past the buffer
/// is clamped to the bytes available; iteration then ends. Stops at a zero-length
/// field or a truncated header.
fn ad_elements(data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut i = 0;
    core::iter::from_fn(move || {
        let length = *data.get(i)? as usize;
        if length == 0 {
            return None;
        }
        let ad_type = *data.get(i + 1)?;
        let value_end = (i + 1 + length).min(data.len());
        let value = data.get(i + 2..value_end).unwrap_or(&[]);
        i += 1 + length;
        Some((ad_type, value))
    })
}

/// Return the first manufacturer-specific data element (AD type `0xFF`) carrying
/// at least a company identifier, or `None` if absent.
fn parse_mfg_from_ad(data: &[u8]) -> Option<ManufacturerData> {
    let value = ad_elements(data)
        .find(|(ad_type, value)| *ad_type == 0xFF && value.len() >= MFG_COMPANY_ID_LEN)
        .map(|(_, value)| value)?;
    let company_id = u16::from_le_bytes([value[0], value[1]]);
    let data_bytes = &value[MFG_COMPANY_ID_LEN..];
    let end = data_bytes.len().min(MFG_PAYLOAD_CAP);
    let payload = heapless::Vec::from_slice(&data_bytes[..end]).unwrap_or_default();
    Some(ManufacturerData {
        company_identifier: company_id,
        payload,
    })
}

/// Bluetooth SIG base UUID suffix, i.e. bytes 4..16 of `0000xxxx-0000-1000-8000-00805F9B34FB`.
/// Used to canonicalize a 32-bit UUID into 128-bit form the same way other stacks do internally,
/// so a `service_uuid` filter matches identically regardless of which AD width a peripheral
/// chose to advertise Service Data with.
const BLE_BASE_UUID_SUFFIX: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB,
];

/// Canonicalizes a 32-bit UUID into [`BleUuid`], demoting it to [`BleUuid::Bit16`] when its
/// top 16 bits are zero. Mirrors `uuid_convert` on the Linux backend (so that their effect is the
/// same), which always canonicalizes through `BlueZ`'s 128-bit `Device1` properties regardless of
/// the AD width the peripheral originally advertised.
fn uuid32_to_ble_uuid(value: u32) -> BleUuid {
    if let Ok(short) = u16::try_from(value) {
        return BleUuid::Bit16(short);
    }
    let mut bytes = [0; 16];
    bytes[..4].copy_from_slice(&value.to_be_bytes());
    bytes[4..].copy_from_slice(&BLE_BASE_UUID_SUFFIX);
    BleUuid::Bit128(bytes)
}

/// Return the first Service Data element - AD type `0x16` (16-bit UUID), `0x20` (32-bit
/// UUID), or `0x21` (128-bit UUID) - or `None` if absent. All three widths are canonicalized
/// to the shared [`BleUuid`] representation so a `service_uuid` filter matches identically
/// on both platforms.
fn parse_service_data_from_ad(data: &[u8]) -> Option<ServiceData> {
    for (ad_type, value) in ad_elements(data) {
        let (uuid, data_bytes) = match ad_type {
            0x16 if value.len() >= 2 => {
                let uuid = BleUuid::Bit16(u16::from_le_bytes([value[0], value[1]]));
                (uuid, &value[2..])
            }
            0x20 if value.len() >= 4 => {
                let raw = u32::from_le_bytes([value[0], value[1], value[2], value[3]]);
                (uuid32_to_ble_uuid(raw), &value[4..])
            }
            0x21 if value.len() >= 16 => {
                // AD stores 128-bit UUIDs little-endian; Bit128 is big-endian.
                let mut be: [u8; 16] = value[..16].try_into().unwrap_or_default();
                be.reverse();
                (BleUuid::Bit128(be), &value[16..])
            }
            _ => continue,
        };
        let end = data_bytes.len().min(SERVICE_DATA_PAYLOAD_CAP);
        let payload = heapless::Vec::from_slice(&data_bytes[..end]).unwrap_or_default();
        return Some(ServiceData { uuid, payload });
    }

    None
}

fn filter_matches(device: &DiscoveredDevice, filter: &DiscoveryFilter) -> bool {
    let advertisement = &device.advertisement;

    if let Some(company_id) = filter.company_id {
        match &advertisement.manufacturer_data {
            Some(mfg) if mfg.company_identifier == company_id => {}
            _ => return false,
        }
    }
    if let Some(ref name_filter) = filter.local_name {
        match &advertisement.local_name {
            Some(name) if name == name_filter => {}
            _ => return false,
        }
    }
    if let Some(ref uuid) = filter.service_uuid {
        let in_uuid_list = advertisement.service_uuids.contains(uuid);
        let in_service_data = advertisement
            .service_data
            .as_ref()
            .is_some_and(|data| &data.uuid == uuid);
        if !in_uuid_list && !in_service_data {
            return false;
        }
    }
    true
}

/// Collect the advertised Service UUIDs into the wire list, capped at
/// [`MAX_ADVERTISED_SERVICE_UUIDS`].
///
/// Handles AD types 0x02/0x03 (16-bit) and 0x06/0x07 (128-bit). BLE
/// advertisements store 128-bit UUIDs in little-endian byte order while
/// `Uuid::Bit128` stores them in big-endian (UUID string) order, so those bytes
/// are reversed; 16-bit UUIDs are little-endian scalars.
fn parse_service_uuids_from_ad(
    data: &[u8],
) -> heapless::Vec<BleUuid, MAX_ADVERTISED_SERVICE_UUIDS> {
    let mut out = heapless::Vec::new();
    for (ad_type, value) in ad_elements(data) {
        match ad_type {
            0x02 | 0x03 => {
                for chunk in value.as_chunks::<2>().0 {
                    let uuid = BleUuid::Bit16(u16::from_le_bytes(*chunk));
                    if out.push(uuid).is_err() {
                        return out;
                    }
                }
            }
            0x06 | 0x07 => {
                for chunk in value.as_chunks::<16>().0 {
                    // AD stores 128-bit UUIDs little-endian; Bit128 is big-endian.
                    let mut be = *chunk;
                    be.reverse();
                    if out.push(BleUuid::Bit128(be)).is_err() {
                        return out;
                    }
                }
            }
            _ => {}
        }
    }

    out
}

/// Return the Complete (0x09) or Shortened (0x08) Local Name, whichever appears
/// first. Returns `None` if absent or non-UTF-8.
fn parse_local_name_from_ad(data: &[u8]) -> Option<heapless::String<32>> {
    let value = ad_elements(data)
        .find(|(ad_type, _)| *ad_type == 0x09 || *ad_type == 0x08)
        .map(|(_, value)| value)?;
    let s = core::str::from_utf8(value).ok()?;
    heapless::String::try_from(s).ok()
}

async fn do_connect(
    ctx: &mut BleContext,
    address: Address,
) -> Result<BTreeMap<BleUuid, WasmService>, Error> {
    if ctx.radio == RadioState::Scanning {
        ctx.scanner
            .as_mut()
            .unwrap()
            .stop_scan()
            .map_err(|_| Error::Generic)?;
        ctx.sub = None;
        ctx.radio = RadioState::Idle;
    }

    let peripheral = Peripheral::new(address_to_ble_addr(&address));

    peripheral.connect().await.map_err(|e| match e {
        ConnectError::Timeout => Error::Timeout,
        _ => Error::Generic,
    })?;

    peripheral
        .discover_all_services()
        .await
        .map_err(|_| Error::Generic)?;

    let svc_list = peripheral.services();
    ctx.char_cache.clear();

    // Build the UUID-keyed `gatt_services` map the guest deserializes, and cache
    // each `Characteristic` (with its ATT handle) for later read/write/notify.
    let mut gatt_services = BTreeMap::new();
    for svc in svc_list.iter() {
        let svc_uuid = svc.uuid();
        let svc_uuid_ble = uuid_to_ble_uuid(svc_uuid);

        let mut characteristics = BTreeMap::new();
        for ch in svc.characteristics().iter() {
            let char_uuid_ble = uuid_to_ble_uuid(ch.uuid());
            characteristics.insert(
                char_uuid_ble,
                WasmChar {
                    uuid: char_uuid_ble,
                    service_uuid: svc_uuid_ble,
                },
            );
            ctx.char_cache.push(CharEntry {
                svc_uuid,
                characteristic: ch.clone(),
            });
        }

        gatt_services.insert(svc_uuid_ble, WasmService { characteristics });
    }

    // Subscribe to peripheral-level events so we can detect disconnection in
    // char_wait_for_notif without blocking forever on a dead notification channel.
    ctx.events_sub = Some(peripheral.events().map_err(|_| Error::Generic)?);
    ctx.peripheral = Some(peripheral);
    ctx.radio = RadioState::Connected;

    Ok(gatt_services)
}

fn find_char<'a>(cache: &'a [CharEntry], char: &WasmChar) -> Option<&'a Characteristic> {
    let char_uuid = ble_uuid_to_uuid(char.uuid);
    let svc_uuid = ble_uuid_to_uuid(char.service_uuid);
    cache
        .iter()
        .find(|e| e.svc_uuid == svc_uuid && e.characteristic.uuid() == char_uuid)
        .map(|e| &e.characteristic)
}

/// Drop all per-connection state.
///
/// Does not call `disconnect()`: every caller reaches this after the link is
/// already down (a disconnect error/event) or after an explicit `disconnect()`,
/// so there is no live controller slot to reclaim.
pub(crate) fn reset_connection(ctx: &mut BleContext) {
    ctx.peripheral = None;
    ctx.char_cache.clear();
    ctx.notif_sub = None;
    ctx.events_sub = None;
    ctx.radio = RadioState::Idle;
}

/// Reads a characteristic. On `Error::Disconnected` `ctx` is left untouched;
/// the caller must reconcile its own connection bookkeeping and tear down the
/// link (see [`reset_connection`]).
pub(crate) async fn char_read(ctx: &mut BleContext, char: WasmChar) -> Result<Vec<u8>, Error> {
    let ch = find_char(&ctx.char_cache, &char)
        .ok_or(Error::Generic)?
        .clone();
    let result = if let Some(p) = &ctx.peripheral {
        p.read(&ch).await
    } else {
        return Err(Error::Generic);
    };
    match result {
        Ok(data) => Ok(data.to_vec()),
        Err(GattError::ReadFailed(
            NimbleError::AttInsufficientAuthen
            | NimbleError::AttInsufficientEnc
            | NimbleError::AttInsufficientAuthor,
        )) => Err(Error::RequiresSecurity),
        Err(GattError::NotConnected | GattError::DisconnectedWhileOperation) => {
            Err(Error::Disconnected)
        }
        Err(_) => Err(Error::Generic),
    }
}

/// Writes a characteristic. On `Error::Disconnected` `ctx` is left untouched;
/// the caller must reconcile its own connection bookkeeping and tear down the
/// link (see [`reset_connection`]).
pub(crate) async fn char_write(
    ctx: &mut BleContext,
    char: WasmChar,
    data: Vec<u8>,
    with_response: bool,
) -> Result<(), Error> {
    let ch = find_char(&ctx.char_cache, &char)
        .ok_or(Error::Generic)?
        .clone();
    let result = if let Some(p) = &ctx.peripheral {
        p.write(&ch, &data, with_response).await
    } else {
        return Err(Error::Generic);
    };
    match result {
        Ok(()) => Ok(()),
        Err(GattError::WriteFailed(
            NimbleError::AttInsufficientAuthen
            | NimbleError::AttInsufficientEnc
            | NimbleError::AttInsufficientAuthor,
        )) => Err(Error::RequiresSecurity),
        Err(GattError::NotConnected | GattError::DisconnectedWhileOperation) => {
            reset_connection(ctx);
            Err(Error::Generic)
        }
        Err(_) => Err(Error::Generic),
    }
}

/// Enables notifications on a characteristic. On `Error::Disconnected` `ctx`
/// is left untouched; the caller must reconcile its own connection
/// bookkeeping and tear down the link (see [`reset_connection`]).
pub(crate) async fn char_register_notif(ctx: &mut BleContext, char: WasmChar) -> Result<(), Error> {
    let ch = find_char(&ctx.char_cache, &char)
        .ok_or(Error::Generic)?
        .clone();
    let cccd = ch
        .descriptors()
        .iter()
        .find(|d| d.uuid() == uuid_from_u16(0x2902))
        .ok_or(Error::Generic)?
        .clone();
    let result = if let Some(p) = &ctx.peripheral {
        p.write_descriptor(&cccd, &[0x01, 0x00], true).await
    } else {
        return Err(Error::Generic);
    };
    match result {
        Ok(()) => Ok(()),
        Err(GattError::NotConnected | GattError::DisconnectedWhileOperation) => {
            Err(Error::Disconnected)
        }
        Err(_) => Err(Error::Generic),
    }
}

/// Disables notifications on a characteristic. Leaves the peripheral-wide
/// notification subscriber ([`ensure_notif_sub`]) untouched: it is shared
/// across every characteristic and only the caller ([`ble_task`](super::ble_task))
/// knows whether other subscriptions remain, via [`clear_notif_sub`].
pub(crate) async fn char_unregister(ctx: &mut BleContext, char: WasmChar) {
    let ch = match find_char(&ctx.char_cache, &char) {
        Some(c) => c.clone(),
        None => return,
    };
    let cccd = match ch
        .descriptors()
        .iter()
        .find(|d| d.uuid() == uuid_from_u16(0x2902))
    {
        Some(d) => d.clone(),
        None => return,
    };
    if let Some(p) = &ctx.peripheral {
        let _ = p.write_descriptor(&cccd, &[0x00, 0x00], true).await;
    }
}

/// Whether a peripheral-wide notification subscriber is currently active.
/// Used by [`ble_task`](super::ble_task) to decide which stream to poll.
pub(crate) fn has_notif_sub(ctx: &BleContext) -> bool {
    ctx.notif_sub.is_some()
}

/// Drops the peripheral-wide notification subscriber. Only safe to call once
/// the last characteristic subscription has been removed (see
/// [`char_unregister`]) — the subscriber is shared across every subscribed
/// characteristic.
pub(crate) fn clear_notif_sub(ctx: &mut BleContext) {
    ctx.notif_sub = None;
}

/// Convert a `myrmic_common::types::ble::Address` to a NimBLE [`BleAddr`].
///
/// `myrmic_common::types::ble::Address.octets` is big-endian (`octets[0]` = MSB).
/// `BleAddr.addr` is little-endian (`addr[0]` = LSB). The bytes are reversed.
/// `public == true` maps to NimBLE address type 0 (public); false maps to 1 (random).
pub(crate) fn address_to_ble_addr(addr: &Address) -> BleAddr {
    let mut octets = addr.octets();
    octets.reverse();
    // NimBLE address type: 0 = public, 1 = random.
    BleAddr::new(u8::from(!addr.is_public()), octets)
}

/// Convert a NimBLE [`BleAddr`] to a `myrmic_common::types::ble::Address`.
pub(crate) fn ble_addr_to_address(ble: &BleAddr) -> Address {
    let mut octets = ble.addr;
    octets.reverse();
    Address::new(octets, ble.type_ == 0)
}

/// Convert a `uuid::Uuid` (esp-nimble-host) to a `myrmic_common::types::ble::Uuid`.
///
/// Always produces `Bit128` — the WASM guest compares UUIDs against full
/// 128-bit values built with `uuid128!`, so `Bit16` would never match.
/// Both types store bytes in big-endian order; no reversal needed.
pub(crate) fn uuid_to_ble_uuid(uuid: Uuid) -> BleUuid {
    BleUuid::Bit128(uuid.into_bytes())
}

/// Convert a `myrmic_common::types::ble::Uuid` to a `uuid::Uuid` (esp-nimble-host).
///
/// `Bit16` short UUIDs are expanded to the standard Bluetooth Base UUID.
pub(crate) fn ble_uuid_to_uuid(uuid: BleUuid) -> Uuid {
    match uuid {
        BleUuid::Bit128(bytes) => Uuid::from_bytes(bytes),
        BleUuid::Bit16(short) => uuid_from_u16(short),
    }
}

/// Outcome of waiting for the next notification or peripheral event.
pub(crate) enum NotifOutcome {
    Message { handle: u16, payload: Vec<u8> },
    Disconnected,
}

/// Awaits the next advertisement matching `filter`. Assumes [`ensure_scanning`]
/// has run so `ctx.sub` is live. Interruptible: the BLE task `select`s this
/// against its command channel, so a `stop` drops the wait between adverts.
pub(crate) async fn next_matching_advert(
    ctx: &mut BleContext,
    filter: &DiscoveryFilter,
) -> DiscoveredDevice {
    loop {
        match ctx.sub.as_mut().unwrap().next_message().await {
            WaitResult::Message(raw) => {
                let device = raw_to_discovered(&raw);
                let device = merge_advertisement(ctx, device);
                if filter_matches(&device, filter) {
                    return device;
                }
            }
            WaitResult::Lagged(_) => {}
        }
    }
}

/// Bound on [`BleContext::adv_cache`]. Only needs to bridge the primary
/// advertisement and scan response for devices currently being evaluated
/// against the active scan filter, not track every device ever seen.
const ADV_CACHE_CAP: usize = 8;

/// A cached, merged view of every advertisement field seen so far for one address.
struct AdvCacheEntry {
    address: Address,
    advertisement: Advertisement,
}

/// Merges `device`'s advertisement fields into the cached view for its address, so
/// data split across the primary advertisement (`ADV_IND`) and the scan response
/// (`SCAN_RSP`) - two separate reports for the same device - ends up combined into
/// one [`Advertisement`] before filtering. Each field falls back to the previously
/// cached value when the new report doesn't carry it; a full cache evicts the
/// oldest entry (FIFO) to make room.
fn merge_advertisement(ctx: &mut BleContext, device: DiscoveredDevice) -> DiscoveredDevice {
    let DiscoveredDevice {
        address,
        advertisement: new,
    } = device;

    let merged = if let Some(entry) = ctx.adv_cache.iter_mut().find(|e| e.address == address) {
        let cached = &mut entry.advertisement;
        if new.local_name.is_some() {
            cached.local_name = new.local_name;
        }
        if new.manufacturer_data.is_some() {
            cached.manufacturer_data = new.manufacturer_data;
        }
        if !new.service_uuids.is_empty() {
            cached.service_uuids = new.service_uuids;
        }
        if new.service_data.is_some() {
            cached.service_data = new.service_data;
        }
        cached.clone()
    } else {
        if ctx.adv_cache.is_full() {
            ctx.adv_cache.remove(0);
        }
        let _ = ctx.adv_cache.push(AdvCacheEntry {
            address,
            advertisement: new.clone(),
        });
        new
    };

    DiscoveredDevice {
        address,
        advertisement: merged,
    }
}

/// Connects and returns the postcard-serialized [`ConnectionInfo`] payload for
/// the `on_connected` callback.
pub(crate) async fn connect_build_info(
    ctx: &mut BleContext,
    address: Address,
    id: u32,
) -> Result<Vec<u8>, Error> {
    let gatt_services = do_connect(ctx, address).await?;
    let info = ConnectionInfo {
        id,
        mac_address: address,
        gatt_services,
    };

    postcard::to_allocvec(&info).map_err(|_| Error::Generic)
}

/// Ensures a peripheral-wide notification subscriber exists (all characteristic
/// notifications arrive on it, dispatched by ATT handle).
pub(crate) fn ensure_notif_sub(ctx: &mut BleContext) -> Result<(), Error> {
    if ctx.notif_sub.is_none() {
        let sub = ctx
            .peripheral
            .as_ref()
            .ok_or(Error::Generic)?
            .subscribe()
            .map_err(|_| Error::Generic)?;
        ctx.notif_sub = Some(sub);
    }

    Ok(())
}

/// Awaits the next notification (from any subscribed characteristic) or a
/// peripheral disconnect. Interruptible via `select` against the command channel.
pub(crate) async fn wait_notification(ctx: &mut BleContext) -> NotifOutcome {
    loop {
        let notif_fut = ctx.notif_sub.as_mut().unwrap().next_message();
        let outcome = if let Some(ev) = ctx.events_sub.as_mut() {
            select(notif_fut, ev.next_message()).await
        } else {
            Either::First(notif_fut.await)
        };
        match outcome {
            Either::First(WaitResult::Message((handle, payload))) => {
                return NotifOutcome::Message { handle, payload };
            }
            Either::First(WaitResult::Lagged(_)) => {}
            Either::Second(WaitResult::Message(PeripheralEvent::Disconnected { .. })) => {
                return NotifOutcome::Disconnected;
            }
            Either::Second(_) => {}
        }
    }
}

/// Awaits a peripheral disconnect event on a connection with no active
/// notification subscriber (otherwise [`wait_notification`] already covers
/// this). Lets [`ble_task`](super::ble_task) detect a remote disconnect on an
/// idle, unsubscribed connection instead of never observing it.
pub(crate) async fn wait_disconnect(ctx: &mut BleContext) {
    loop {
        let Some(ev) = ctx.events_sub.as_mut() else {
            return;
        };
        if let WaitResult::Message(PeripheralEvent::Disconnected { .. }) = ev.next_message().await {
            return;
        }
    }
}

/// The ATT handle of a cached characteristic, used to dispatch notifications.
pub(crate) fn char_handle(ctx: &BleContext, char: &WasmChar) -> Option<u16> {
    find_char(&ctx.char_cache, char).map(Characteristic::handle)
}

/// Stops scanning if active and drops the advertisement subscription.
pub(crate) fn stop_scanning(ctx: &mut BleContext) {
    if ctx.radio == RadioState::Scanning {
        if let Some(scanner) = ctx.scanner.as_mut() {
            let _ = scanner.stop_scan();
        }
        ctx.sub = None;
        ctx.radio = RadioState::Idle;
        ctx.adv_cache.clear();
    }
}

/// Disconnects the active peripheral (if any) and clears connection state.
pub(crate) async fn disconnect_active(ctx: &mut BleContext) {
    if let Some(p) = &ctx.peripheral {
        let _ = p.disconnect().await;
    }
    reset_connection(ctx);
}

/// Enables pairing on the active connection with a static passkey.
pub(crate) async fn set_passkey(ctx: &BleContext, passkey: u32) -> Result<(), Error> {
    match &ctx.peripheral {
        Some(p) => p
            .pair_with_passkey(passkey)
            .await
            .map_err(|_| Error::Generic),
        None => Err(Error::Generic),
    }
}
