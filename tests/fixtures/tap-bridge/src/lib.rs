//! Tap-bridge cell for signal-layer (dataplane) HIL tests.
//!
//! The `signal-layer-logger` cell prints taps to the serial console, which the
//! HIL harness cannot assert on — it asserts over Zenoh. This cell bridges the
//! tap interface to Zenoh events.
//!
//! Commands are fire-and-forget (there is no synchronous reply anymore), so each
//! command publishes its result as a Zenoh event the host subscribes to:
//!
//! - **`read_tap` command** — payload is a tap name (UTF-8). Publishes a
//!   `tap_value` event with a 1-byte status prefix: `[1][ts_ms: u64 LE][value:
//!   f32 LE]` (13 bytes) when the tap holds a value, or `[0]` (1 byte) when the
//!   tap is unknown or holds no value (e.g. cleared after a driver fault).
//! - **`tap_names` command** — publishes every registered tap name, `\n`
//!   separated, on the `tap_names` event. Used by the discovery test.
//! - **`drain_events` command** — drains the event taps the suite cares about
//!   (`_signal_layer_health`, `sim_alarm`) and republishes each raw payload as
//!   a Zenoh event (`tap_health`, `tap_alarm`). The host drives this on demand
//!   and asserts on the republished events.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use myrmic_sdk::tap::Tap;
use myrmic_sdk::{Bytes, EventPublishRequest, Metadata, Result, error, publish_event};

/// Max tap-name length we handle (matches the host registry's name budget).
const MAX_NAME: usize = 64;
/// Upper bound on taps we enumerate for `tap_names`.
const MAX_TAPS: u32 = 32;
/// Scratch buffer for a single drained event payload (postcard-encoded).
const MAX_EVENT: usize = 64;

/// Event taps drained and republished: `(tap name, Zenoh event name)`.
const EVENT_TAPS: &[(&str, &str)] = &[
    ("_signal_layer_health", "tap_health"),
    ("sim_alarm", "tap_alarm"),
];

/// `read_tap` — payload is a tap name; publishes `[1][ts:8][val:4]` (value
/// present) or `[0]` (no value) on the `tap_value` event.
#[myrmic_sdk::cmd]
fn read_tap(_md: Metadata, name: Bytes) -> Result<()> {
    let reply = read_present_value(&name).unwrap_or_else(|| alloc::vec![0u8]);
    publish_event(&EventPublishRequest {
        event: "tap_value".try_into()?,
        payload: Some(reply),
    })?;
    Ok(())
}

/// Returns `Some([1][ts:8][val:4])` when the requested tap holds an `f32`, or
/// `None` for any reason the tap has no readable value (logged, not propagated).
fn read_present_value(name: &[u8]) -> Option<Vec<u8>> {
    let name = core::str::from_utf8(name).ok()?;

    match Tap::resolve(name) {
        Ok(Some(tap)) => match tap.read_typed::<f32>() {
            Ok(Some((ts_ms, value))) => {
                let mut out = Vec::with_capacity(13);
                out.push(1u8); // status: value present
                out.extend_from_slice(&ts_ms.to_le_bytes());
                out.extend_from_slice(&value.to_le_bytes());
                Some(out)
            }
            // Tap exists but holds no value, or the read/decode failed.
            Ok(None) => None,
            Err(e) => {
                let _ = error!("read_tap '{name}': read error {e:?}");
                None
            }
        },
        Ok(None) => None, // unknown tap name
        Err(e) => {
            let _ = error!("read_tap '{name}': resolve error {e:?}");
            None
        }
    }
}

/// `tap_names` — publishes every registered tap name, `\n` separated, on the
/// `tap_names` event.
#[myrmic_sdk::cmd]
fn tap_names(_md: Metadata) -> Result<()> {
    let count = match myrmic_sdk::tap::list_len() {
        Ok(n) => n.min(MAX_TAPS),
        Err(e) => {
            let _ = error!("tap_names: list_len failed: {e:?}");
            0
        }
    };
    let mut out: Vec<u8> = Vec::new();
    for i in 0..count {
        let mut name_buf = [0u8; MAX_NAME];
        match myrmic_sdk::tap::list_entry(i, &mut name_buf) {
            Ok(Some((len, _kind))) => {
                if !out.is_empty() {
                    out.push(b'\n');
                }
                out.extend_from_slice(&name_buf[..len]);
            }
            Ok(None) => {} // index past the end — skip
            Err(e) => {
                let _ = error!("tap_names: list_entry({i}) failed: {e:?}");
                break;
            }
        }
    }
    publish_event(&EventPublishRequest {
        event: "tap_names".try_into()?,
        payload: Some(out),
    })?;
    Ok(())
}

/// `drain_events` — drain the event taps and republish each payload over Zenoh.
#[myrmic_sdk::cmd]
fn drain_events(_md: Metadata) -> Result<()> {
    for &(tap_name, event_name) in EVENT_TAPS {
        let Ok(Some(tap)) = Tap::resolve(tap_name) else {
            continue;
        };
        let mut buf = [0u8; MAX_EVENT];
        loop {
            match tap.take_event(&mut buf) {
                Ok(Some(len)) => match event_name.try_into() {
                    Ok(event) => {
                        if let Err(e) = publish_event(&EventPublishRequest {
                            event,
                            payload: Some(buf[..len].to_vec()),
                        }) {
                            let _ = error!("drain_events publish '{event_name}': {e}");
                        }
                    }
                    Err(_) => {
                        let _ = error!("drain_events: bad event name '{event_name}'");
                    }
                },
                Ok(None) => break, // queue drained
                Err(e) => {
                    let _ = error!("drain_events take_event '{tap_name}': {e:?}");
                    break;
                }
            }
        }
    }
    Ok(())
}
