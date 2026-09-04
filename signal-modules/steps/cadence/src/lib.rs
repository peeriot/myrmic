//! Cadence step — throttles how often a path recomputes.
//!
//! Passes one of every `every` samples and, on the in-between ticks, either
//! drops the value ([`Decimate`](CadenceMode::Decimate)) or re-emits the last
//! sampled value ([`SampleHold`](CadenceMode::SampleHold)). This controls how
//! often a downstream Outlet is written, as a per-path property. Generic over
//! the carried value `T`, so it composes anywhere in a chain. Pure — it works by
//! sample count, not wall-clock time (the effective period is
//! `every × sample_interval`; a time-based throttle would need a clock, which
//! the pure `ProcessingStep` trait does not provide).

#![cfg_attr(not(test), no_std)]

use signal_layer_core::ProcessingStep;

/// What a cadence step emits on the ticks between samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CadenceMode {
    /// Emit one of every N samples; emit nothing (`None`) on the rest.
    #[default]
    Decimate,
    /// Emit one of every N samples and re-emit that held value on the rest, so
    /// the downstream is refreshed every tick.
    SampleHold,
}

pub struct CadenceConfig {
    /// Pass 1 of every N samples (`0` or `1` = passthrough).
    pub every: u32,
    /// Behaviour on the ticks between samples.
    pub mode: CadenceMode,
}

pub struct CadenceState<T> {
    every: u32,
    count: u32,
    mode: CadenceMode,
    held: Option<T>,
}

impl<T> CadenceState<T> {
    #[must_use]
    pub fn new(cfg: CadenceConfig) -> Self {
        Self {
            every: cfg.every,
            count: 0,
            mode: cfg.mode,
            held: None,
        }
    }
}

impl<T: Clone> ProcessingStep for CadenceState<T> {
    type Input = T;
    type Output = T;

    fn step(&mut self, input: T) -> Option<T> {
        if self.every <= 1 {
            return Some(input); // passthrough
        }
        // Sample the first of each window; count the rest.
        let is_sample = self.count == 0;
        self.count = (self.count + 1) % self.every;
        match self.mode {
            CadenceMode::Decimate => {
                if is_sample {
                    Some(input)
                } else {
                    None
                }
            }
            CadenceMode::SampleHold => {
                if is_sample {
                    self.held = Some(input.clone());
                    Some(input)
                } else {
                    self.held.clone()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimate(every: u32) -> CadenceConfig {
        CadenceConfig {
            every,
            mode: CadenceMode::Decimate,
        }
    }

    #[test]
    fn passthrough_when_every_is_one() {
        let mut c: CadenceState<u32> = CadenceState::new(decimate(1));
        assert_eq!(c.step(1), Some(1));
        assert_eq!(c.step(2), Some(2));
        assert_eq!(c.step(3), Some(3));
    }

    #[test]
    fn decimates_one_in_three() {
        let mut c: CadenceState<u32> = CadenceState::new(decimate(3));
        assert_eq!(c.step(1), Some(1)); // first of window
        assert_eq!(c.step(2), None);
        assert_eq!(c.step(3), None);
        assert_eq!(c.step(4), Some(4)); // next window
        assert_eq!(c.step(5), None);
    }

    #[test]
    fn zero_is_treated_as_passthrough() {
        let mut c: CadenceState<f32> = CadenceState::new(decimate(0));
        assert_eq!(c.step(1.0), Some(1.0));
        assert_eq!(c.step(2.0), Some(2.0));
    }

    #[test]
    fn sample_hold_reemits_last_between_samples() {
        let mut c: CadenceState<u32> = CadenceState::new(CadenceConfig {
            every: 3,
            mode: CadenceMode::SampleHold,
        });
        assert_eq!(c.step(1), Some(1)); // sampled
        assert_eq!(c.step(2), Some(1)); // held
        assert_eq!(c.step(3), Some(1)); // held
        assert_eq!(c.step(4), Some(4)); // re-sampled
        assert_eq!(c.step(5), Some(4)); // held
    }
}
