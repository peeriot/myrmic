//! BLE adapter for the Thermobeacon Mini Hygrometer
//!
//! The Thermobeacon Mini (sold as Thermoplus / Brifit / Oria) broadcasts temperature, humidity and
//! battery voltage passively in the Manufacturer Data of its primary advertisement, so this adapter
//! never needs a GATT connection: a filtered passive scan is enough. Sending `enable` starts that
//! scan together with a periodic publish timer; each matching advertisement arrives as a
//! `discovered` invocation, which decodes the reading and buffers it. Every publish interval
//! `publish_agg` summarises the buffered readings and publishes them as one `thermobeacon_sensor`
//! event. Sending `disable` stops the scan and the timer again.
//!
//! The wire format and scaling are decoded in [`protocol`], ported from the ble-monitor project's
//! Thermobeacon parser.
//!
//! The scan and timer handles are the only long-lived host resources this adapter holds. They live
//! in an [`InMemory`] and are never persisted, so after a restart the cell is idle again and waits
//! for another `enable`.
//!
//! Deploy and drive it with myrmic:
//! ```shell
//! $ myrmic build
//! $ myrmic runtimes start -d
//! $ myrmic deploy
//! $ myrmic send <sri> enable
//! $ myrmic telemetry logs
//! $ myrmic send <sri> disable
//! ```
#![no_std]

mod protocol;

use core::time::Duration;
use myrmic_sdk::ble::{self, DiscoveredDevice, ScanHandle};
use myrmic_sdk::{
    Callback, DiscoveryFilter, InMemory, Metadata, ScanMode, TimerHandle, interval, publish,
};

use crate::protocol::{Measurement, aggregate, parse_measurement};

/// Thermobeacon Mini company identifier (`0x0010` = 16), carried in its manufacturer data. The SDK
/// filters on this and hands `discovered` the payload with these two bytes already stripped.
const THERMOBEACON_COMPANY_ID: u16 = 0x0010;
/// Publish interval used when `init` is given no `publish_interval_s`.
const DEFAULT_PUBLISH_INTERVAL_S: u32 = 5;

/// Holds the running scan and publish timer while the adapter is enabled. Dropping a [`ScanHandle`]
/// or [`TimerHandle`] does not stop it, so both are kept here rather than discarded, and `disable`
/// takes them back out to stop them. Also carries the interval `enable` should use for the timer.
static STATE: InMemory<State> = InMemory::new(State::new());

struct State {
    scan: Option<ScanHandle>,
    timer: Option<TimerHandle>,
    publish_interval_s: u32,
    /// Set when `discovered` buffers a reading, cleared when `publish_agg` publishes; lets a tick
    /// skip publishing when no advertisement arrived since the last one.
    fresh: bool,
}

impl State {
    pub const fn new() -> Self {
        Self {
            scan: None,
            timer: None,
            publish_interval_s: DEFAULT_PUBLISH_INTERVAL_S,
            fresh: false,
        }
    }
}

static MEASUREMENTS: InMemory<heapless::HistoryBuf<Measurement, 16>> =
    InMemory::new(heapless::HistoryBuf::<Measurement, 16>::new());

/// Records the configured publish interval and waits for `enable`; starts nothing on its own.
#[myrmic_sdk::init]
fn init(md: Metadata, publish_interval_s: Option<u32>) -> myrmic_sdk::Result {
    let publish_interval_s = publish_interval_s.unwrap_or(DEFAULT_PUBLISH_INTERVAL_S);
    STATE.with(|state| state.publish_interval_s = publish_interval_s)?;

    myrmic_sdk::info!(
        "Thermobeacon Mini adapter loaded (id={:?}); send `enable` to start scanning",
        md.id
    )?;

    Ok(())
}

/// Starts the passive scan and the periodic publish timer. Does nothing if already enabled.
#[myrmic_sdk::cmd]
fn enable(_md: Metadata) -> myrmic_sdk::Result {
    let publish_interval_s = STATE.with(|state| {
        if state.scan.is_some() {
            None
        } else {
            Some(state.publish_interval_s)
        }
    })?;

    let Some(publish_interval_s) = publish_interval_s else {
        myrmic_sdk::info!("Thermobeacon Mini adapter already enabled")?;
        return Ok(());
    };

    // Everything the sensor reports is in its primary advertisement, so there is no scan response
    // to request and a passive scan is enough.
    let scan = ble::scan(
        Callback::of::<discovered>(),
        Some(DiscoveryFilter {
            company_id: Some(THERMOBEACON_COMPANY_ID),
            local_name: None,
            service_uuid: None,
        }),
        ScanMode::Passive,
    )?;

    let timer = interval(
        Callback::of::<publish_agg>(),
        Duration::from_secs(publish_interval_s as u64),
    )
    .build()?;

    STATE.with(|state| {
        state.scan = Some(scan);
        state.timer = Some(timer);
        state.fresh = false;
    })?;

    myrmic_sdk::info!("Thermobeacon Mini adapter enabled")?;

    Ok(())
}

/// Stops the scan and publish timer and drops any buffered readings, so a later `enable` starts a
/// fresh window. Does nothing if already disabled.
#[myrmic_sdk::cmd]
fn disable(_md: Metadata) -> myrmic_sdk::Result {
    let (scan, timer) = STATE.with(|state| {
        state.fresh = false;
        (state.scan.take(), state.timer.take())
    })?;

    if let Some(scan) = scan {
        scan.stop()?;
    }
    if let Some(timer) = timer {
        timer.cancel()?;
    }
    MEASUREMENTS.with(|m| m.clear())?;

    myrmic_sdk::info!("Thermobeacon Mini adapter disabled")?;

    Ok(())
}

/// Fired by the publish timer once per interval: when at least one advertisement has been decoded
/// since the last tick, re-aggregates the readings currently buffered — a rolling window of the
/// most recent readings, not only those since the last tick, since the buffer is not cleared per
/// tick — and publishes the summary as one `thermobeacon_sensor` event. Skips the tick when no
/// advertisement arrived since the last one, so a silent sensor stops producing events rather than
/// re-publishing its last window. Runs on the timer's `Callback<Void>`, so it takes no payload.
#[myrmic_sdk::cmd]
fn publish_agg(_md: Metadata) -> myrmic_sdk::Result {
    if !STATE.with(|state| core::mem::take(&mut state.fresh))? {
        return Ok(());
    }

    let Some(agg) = MEASUREMENTS.with(|m| aggregate(m.oldest_ordered()))? else {
        return Ok(());
    };

    let payload = serde_json::to_vec(&agg).map_err(|_| "failed to serialize sensor event")?;

    publish("thermobeacon_sensor", &payload)?;

    Ok(())
}

/// Fired for every advertisement matching the filter. Decodes the reading and buffers it; the scan
/// is left running, so this is the steady-state path rather than a one-off.
#[myrmic_sdk::cmd]
fn discovered(_md: Metadata, device: DiscoveredDevice) -> myrmic_sdk::Result {
    let Some(manufacturer_data) = &device.advertisement.manufacturer_data else {
        return Ok(());
    };

    match parse_measurement(&manufacturer_data.payload) {
        Ok(measurement) => {
            myrmic_sdk::info!("Reading from {}:\n{measurement}", device.address)?;

            MEASUREMENTS.with(|m| m.write(measurement))?;
            STATE.with(|state| state.fresh = true)?;
        }
        Err(err) => {
            myrmic_sdk::warn!("Ignoring advertisement from {}: {err}", device.address)?;
        }
    }

    Ok(())
}
