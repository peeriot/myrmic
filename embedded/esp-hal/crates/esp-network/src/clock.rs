//! Hybrid logical clock synced from the swarm.
//!
//! The device has no wall clock. Wall time is learned from the first
//! timestamped zenoh message received from the swarm: monotonic uptime plus
//! that learned offset then drives the HLC's physical clock. Until then
//! [`SwarmClock`] stamps nothing, so pre-sync (near-epoch-zero) timestamps
//! never leave the device.

use core::cell::Cell;
use core::time::Duration;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use uhlc::{HLC, HLCBuilder, NTP64, Timestamp};
use zenoh_nano::clock::Clock;
use zenoh_nano::scout::ZenohIdProto;

/// Offset from monotonic uptime to swarm wall-clock time, in NTP64 units.
/// 0 until the first timestamped message arrives.
static OFFSET: Mutex<CriticalSectionRawMutex, Cell<u64>> = Mutex::new(Cell::new(0));

/// Maximum accepted forward drift of an incoming timestamp once synced.
const MAX_DELTA: Duration = Duration::from_secs(10);

/// Monotonic uptime as NTP64.
fn uptime() -> NTP64 {
    NTP64::from(Duration::from_micros(
        embassy_time::Instant::now().as_micros(),
    ))
}

/// The HLC's physical clock: uptime shifted to swarm wall-clock time.
fn physical_clock() -> NTP64 {
    NTP64(uptime().0.wrapping_add(OFFSET.lock(Cell::get)))
}

/// Swarm wall-clock time since `UNIX_EPOCH`; `None` until the first sync.
/// Serves the guest's `now()` host call.
pub fn wall_time() -> Option<Duration> {
    (OFFSET.lock(Cell::get) != 0).then(|| physical_clock().to_duration())
}

/// The device's hybrid logical clock, identified by its stable zid.
pub(crate) struct SwarmClock {
    hlc: HLC,
}

impl SwarmClock {
    pub(crate) fn new(zid: ZenohIdProto) -> Self {
        Self {
            hlc: HLCBuilder::new()
                .with_id(zid.into())
                .with_clock(physical_clock)
                .with_max_delta(MAX_DELTA)
                .build(),
        }
    }
}

impl Clock for SwarmClock {
    fn observe(&self, timestamp: &Timestamp) {
        let learned = OFFSET.lock(|offset| {
            if offset.get() != 0 {
                return false;
            }
            offset.set(timestamp.get_time().0.saturating_sub(uptime().0));
            true
        });
        if learned {
            log::info!(
                "clock synced to swarm time ({}s since epoch)",
                timestamp.get_time().as_secs()
            );
        }

        if let Err(err) = self.hlc.update_with_timestamp(timestamp) {
            log::warn!("rejected remote timestamp: {err}");
        }
    }

    fn timestamp(&self) -> Option<Timestamp> {
        (OFFSET.lock(Cell::get) != 0).then(|| self.hlc.new_timestamp())
    }
}
