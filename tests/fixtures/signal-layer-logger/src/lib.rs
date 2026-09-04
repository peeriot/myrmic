#![no_std]

use core::time::Duration;

use myrmic_sdk::signal_layer::{HealthEvent, ThresholdAlarm};
use myrmic_sdk::tap::{Tap, TapKind};
use myrmic_sdk::{Callback, Metadata, Result};

const MAX_NAME: usize = 64;
const MAX_TAPS: usize = 32;

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    myrmic_sdk::interval(Callback::of::<log_taps>(), Duration::from_millis(1000))
        .build()
        .map_err(|_| "failed to create timer")?;
    Ok(())
}

/// Timer target: walks the tap registry and logs each tap's current value/events.
#[myrmic_sdk::cmd]
fn log_taps(_md: Metadata) -> Result<()> {
    let count = myrmic_sdk::tap::list_len()
        .unwrap_or(0)
        .min(MAX_TAPS as u32);

    for i in 0..count {
        let mut name_buf = [0u8; MAX_NAME];
        let Some((name_len, kind)) = myrmic_sdk::tap::list_entry(i, &mut name_buf)? else {
            continue;
        };
        let name = &name_buf[..name_len];
        let name_str = core::str::from_utf8(name).unwrap_or("?");
        let Some(tap) = Tap::resolve(name_str)? else {
            continue;
        };

        match kind {
            TapKind::Retained => {
                match tap.read_typed::<f32>() {
                    Ok(Some((ts_ms, value))) => {
                        let _ = myrmic_sdk::info!(
                            "[signal-layer-logger] {name_str} = {value:.3} (ts={ts_ms}ms)"
                        );
                    }
                    Ok(None) => {}
                    Err(_) => {
                        // Not an f32 or decode failed — skip silently.
                    }
                }
            }
            TapKind::Event => {
                if name_str == "_signal_layer_health" {
                    // Known type: HealthEvent
                    while let Ok(Some(event)) = tap.take_event_typed::<HealthEvent>() {
                        let _ = myrmic_sdk::info!(
                            "[signal-layer-logger] health[{}]: {:?}",
                            event.source,
                            event.state,
                        );
                    }
                } else {
                    // Try ThresholdAlarm; fall back to raw if decode fails.
                    loop {
                        match tap.take_event_typed::<ThresholdAlarm>() {
                            Ok(Some(alarm)) => {
                                let _ = myrmic_sdk::info!(
                                    "[signal-layer-logger] ALARM '{name_str}': value={:.3} threshold={:.3}",
                                    alarm.value,
                                    alarm.threshold,
                                );
                            }
                            Ok(None) => break,
                            Err(e) => {
                                let _ = myrmic_sdk::warn!(
                                    "[signal-layer-logger] '{name_str}': decode error, event dropped: {e:?}"
                                );
                                break;
                            }
                        }
                    }
                }
            }
            TapKind::Batch => {}
            TapKind::Unknown(_) => {}
        }
    }
    Ok(())
}
