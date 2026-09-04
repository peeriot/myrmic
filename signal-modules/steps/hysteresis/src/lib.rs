//! Two-threshold hysteresis controller — feed-forward digital actuator control.
//!
//! Emits a [`DigitalState`] only on a state transition: ON when the input rises
//! to/above `on_threshold`, OFF when it falls to/below `off_threshold`. The gap
//! between the thresholds is the hysteresis band that prevents chattering around
//! a single setpoint. Honestly scoped to threshold/hysteresis feed-forward — no
//! PID, no fixed-dt control claim.

#![cfg_attr(not(test), no_std)]

use signal_layer_core::ProcessingStep;
use signal_layer_types::DigitalState;

pub struct HysteresisConfig {
    /// Assert (ON) when the input reaches or exceeds this value.
    pub on_threshold: f32,
    /// Deassert (OFF) when the input reaches or falls below this value.
    pub off_threshold: f32,
}

pub struct HysteresisState {
    on_threshold: f32,
    off_threshold: f32,
    on: bool,
}

impl HysteresisState {
    #[must_use]
    pub fn new(cfg: HysteresisConfig) -> Self {
        // Normalize so on_threshold >= off_threshold; a swapped config would
        // otherwise chatter (toggle every tick) for inputs between the two.
        let on_threshold = cfg.on_threshold.max(cfg.off_threshold);
        let off_threshold = cfg.on_threshold.min(cfg.off_threshold);
        Self {
            on_threshold,
            off_threshold,
            on: false,
        }
    }
}

impl ProcessingStep for HysteresisState {
    type Input = f32;
    type Output = DigitalState;

    fn step(&mut self, value: f32) -> Option<DigitalState> {
        if !self.on && value >= self.on_threshold {
            self.on = true;
            Some(DigitalState { on: true })
        } else if self.on && value <= self.off_threshold {
            self.on = false;
            Some(DigitalState { on: false })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asserts_above_on_threshold_and_holds() {
        let mut s = HysteresisState::new(HysteresisConfig {
            on_threshold: 30.0,
            off_threshold: 25.0,
        });
        assert_eq!(s.step(20.0), None); // below on_threshold, starts off
        assert_eq!(s.step(30.0), Some(DigitalState { on: true })); // reaches on_threshold
        assert_eq!(s.step(35.0), None); // stays on, no re-emit
    }

    #[test]
    fn deasserts_below_off_threshold() {
        let mut s = HysteresisState::new(HysteresisConfig {
            on_threshold: 30.0,
            off_threshold: 25.0,
        });
        assert_eq!(s.step(31.0), Some(DigitalState { on: true }));
        assert_eq!(s.step(27.0), None); // in the band, holds on
        assert_eq!(s.step(25.0), Some(DigitalState { on: false })); // reaches off_threshold
        assert_eq!(s.step(24.0), None); // stays off
    }

    #[test]
    fn swapped_thresholds_are_normalized_no_chatter() {
        // Misconfigured off > on must normalize to on=30, off=25, not chatter.
        let mut s = HysteresisState::new(HysteresisConfig {
            on_threshold: 25.0,
            off_threshold: 30.0,
        });
        assert_eq!(s.step(31.0), Some(DigitalState { on: true }));
        assert_eq!(s.step(27.0), None); // inside the normalized band, holds on
        assert_eq!(s.step(27.0), None); // no chatter on the next tick
        assert_eq!(s.step(25.0), Some(DigitalState { on: false }));
    }

    #[test]
    fn hysteresis_band_prevents_chatter() {
        let mut s = HysteresisState::new(HysteresisConfig {
            on_threshold: 30.0,
            off_threshold: 25.0,
        });
        assert_eq!(s.step(31.0), Some(DigitalState { on: true }));
        // Oscillating inside the band must not toggle the output.
        assert_eq!(s.step(26.0), None);
        assert_eq!(s.step(29.0), None);
        assert_eq!(s.step(26.0), None);
    }
}
