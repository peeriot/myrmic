//! Shared Signal Layer types - serialized across the host/WASM boundary.
//!
//! All types implement [`serde::Serialize`] / [`serde::Deserialize`] via
//! postcard so they can be written into tap slots on the embedded host and
//! decoded by WASM cells without any allocation.

#![no_std]

mod wire_type;

pub use wire_type::{WireType, fnv1a_32};

use serde::{Deserialize, Serialize};

/// Command payload for a digital on/off outlet (e.g. a relay or a GPIO driven
/// high/low). Written by a cell (or an in-layer step) into an outlet slot and
/// consumed by the backing driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigitalState {
    /// Desired energised state: `true` = on/asserted, `false` = off/deasserted.
    pub on: bool,
}

/// Command payload for a PWM outlet, as a duty-cycle fraction in `0.0..=1.0`.
///
/// The value carried here is only the *declared type* half of validation:
/// the backing PWM driver still has to clamp it to its allowed range and
/// honour its own protective limits before applying it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PwmDuty {
    /// Duty cycle as a fraction of full scale, nominally `0.0..=1.0`.
    pub duty: f32,
}

/// Fault reported by an output device on its error Event tap. Published only on
/// a real failure - a successful command write never emits a fault, and never
/// populates the status tap (status comes only from a real read-back).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutletFault {
    /// Applying a command to the device failed.
    WriteFailed,
    /// Reading the device's status back failed.
    ReadFailed,
}

/// Fired by a threshold step when the monitored value crosses its configured limit.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThresholdAlarm {
    /// The sample value that triggered the alarm.
    pub value: f32,
    /// The threshold that was crossed.
    pub threshold: f32,
}

/// Operational state reported by a driver source task.
///
/// None of these states is terminal: a source task that drops to
/// [`Degraded`](DriverHealth::Degraded) or [`Down`](DriverHealth::Down) keeps
/// running and periodically re-runs the driver's `init()` to bring the sensor
/// back up, transitioning to [`Up`](DriverHealth::Up) once a sample succeeds again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriverHealth {
    /// The driver is initialised and its last sample succeeded.
    Up,
    /// A sample returned an error. The source task re-runs `init()` to recover;
    /// while degraded the source's retained taps are cleared, so reads return
    /// no value rather than a stale one. A later successful sample transitions
    /// back to [`Up`](DriverHealth::Up).
    Degraded,
    /// Bring-up (`init()`) has not yet succeeded - at boot or after a failure.
    /// **Not terminal:** the source task keeps retrying `init()` and transitions
    /// to [`Up`](DriverHealth::Up) once the sensor responds and a sample succeeds.
    Down,
}

/// Health transition event emitted on the `_signal_layer_health` tap.
///
/// Emitted only on state changes - a driver that stays [`DriverHealth::Up`] produces
/// no events. Consumers can reconstruct the current health of each source by
/// tracking the most recent event per [`source`](HealthEvent::source) index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthEvent {
    /// Zero-based index of the source task that changed state, matching the
    /// order in which sources are listed in the pipeline YAML.
    pub source: u8,
    /// The new health state of the source.
    pub state: DriverHealth,
}
