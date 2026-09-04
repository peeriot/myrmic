//! Generic PWM duty-cycle output driver (fan, pump, dimmable LED, …).
//!
//! The write-side counterpart of a sensor driver: it `apply()`s a [`PwmDuty`]
//! (a `0.0..=1.0` duty fraction) onto an owned PWM channel generic over
//! [`embedded_hal::pwm::SetDutyCycle`]. The requested duty is clamped to the
//! configured range as a non-negotiable floor before it reaches the hardware.
//! De-energizing the channel is not a requested duty and bypasses that floor,
//! so a device with a minimum duty can still be turned off.

#![cfg_attr(not(test), no_std)]

use embedded_hal::pwm::SetDutyCycle;
use signal_layer_types::PwmDuty;

/// Hardware-tier configuration for a PWM output.
#[derive(Debug, Clone, Copy)]
pub struct PwmOutputConfig {
    /// Lower duty-cycle clamp (fraction of full scale, `0.0..=1.0`).
    pub min_duty: f32,
    /// Upper duty-cycle clamp (fraction of full scale, `0.0..=1.0`).
    pub max_duty: f32,
    /// Protective floor: the minimum time (ms) between duty updates. A command
    /// arriving sooner is dropped, rate-limiting how fast the duty can change —
    /// enforced here, independent of any pipeline config. `0` disables the limit.
    pub min_update_interval_ms: u64,
}

impl Default for PwmOutputConfig {
    fn default() -> Self {
        Self {
            min_duty: 0.0,
            max_duty: 1.0,
            min_update_interval_ms: 0,
        }
    }
}

/// Error type — a PWM write failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PwmOutputError<E> {
    Pwm(E),
}

impl<E> From<E> for PwmOutputError<E> {
    fn from(e: E) -> Self {
        Self::Pwm(e)
    }
}

/// A PWM output bound to a single duty-cycle channel.
pub struct PwmOutput<P> {
    pwm: P,
    min_duty: f32,
    max_duty: f32,
    min_update_interval_ms: u64,
    /// Timestamp (ms) of the last duty update (`None` until the first).
    last_update_ms: Option<u64>,
}

impl<P: SetDutyCycle> PwmOutput<P> {
    /// Construct the driver around an owned PWM channel. Does not drive the
    /// channel — call [`init`](Self::init) to establish the safe (0%) state.
    pub fn new(cfg: &PwmOutputConfig, pwm: P) -> Self {
        // Normalize so `min_duty <= max_duty`: f32::clamp panics if min > max,
        // and a mis-ordered config must not crash on every apply().
        let (min_duty, max_duty) = if cfg.min_duty <= cfg.max_duty {
            (cfg.min_duty, cfg.max_duty)
        } else {
            (cfg.max_duty, cfg.min_duty)
        };
        Self {
            pwm,
            min_duty,
            max_duty,
            min_update_interval_ms: cfg.min_update_interval_ms,
            last_update_ms: None,
        }
    }

    fn drive(&mut self, duty: f32) -> Result<(), PwmOutputError<P::Error>> {
        self.write_duty(duty.clamp(self.min_duty, self.max_duty).clamp(0.0, 1.0))
    }

    /// De-energize the channel, bypassing `min_duty`.
    ///
    /// `min_duty` is the floor for a *requested* duty, so that a channel is never
    /// asked to run slower than its device can sustain. Off is not a duty: a
    /// channel that cannot reach 0 cannot be de-energized at all, which would
    /// leave a minimum-duty device running from bring-up onward.
    fn drive_off(&mut self) -> Result<(), PwmOutputError<P::Error>> {
        self.write_duty(0.0)
    }

    /// Write an already-resolved duty fraction to the channel.
    fn write_duty(&mut self, duty: f32) -> Result<(), PwmOutputError<P::Error>> {
        let max = self.pwm.max_duty_cycle();
        // Round-to-nearest without std's `f32::round` (unavailable in no_std):
        // duty ∈ [0,1] and max ≤ u16::MAX, so `duty * max + 0.5` ≤ 65535.5 and
        // truncating to u16 is exact and non-negative.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "duty ∈ [0,1], max ≤ u16::MAX ⇒ product+0.5 fits u16 and is non-negative"
        )]
        let raw = (duty * f32::from(max) + 0.5) as u16;
        self.pwm.set_duty_cycle(raw)?;
        Ok(())
    }

    /// Drive the output to 0% duty, bypassing `min_duty` (the protective floor
    /// does not apply to bring-up). Idempotent.
    pub fn init(&mut self) -> Result<(), PwmOutputError<P::Error>> {
        self.drive_off()?;
        self.last_update_ms = None;
        Ok(())
    }

    /// Apply a duty-cycle command at time `now_ms`, clamped to the configured
    /// range (and to the physical `0.0..=1.0`) and scaled to the channel's raw
    /// resolution. Rate-limited: a command arriving within
    /// `min_update_interval_ms` of the last update is dropped.
    pub fn apply(&mut self, cmd: PwmDuty, now_ms: u64) -> Result<(), PwmOutputError<P::Error>> {
        if let Some(last) = self.last_update_ms
            && now_ms.saturating_sub(last) < self.min_update_interval_ms
        {
            return Ok(()); // too soon — hold the current duty
        }
        self.drive(cmd.duty)?;
        self.last_update_ms = Some(now_ms);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal fake PWM channel recording its last raw duty.
    struct FakePwm {
        max: u16,
        duty: u16,
    }

    impl embedded_hal::pwm::ErrorType for FakePwm {
        type Error = core::convert::Infallible;
    }

    impl SetDutyCycle for FakePwm {
        fn max_duty_cycle(&self) -> u16 {
            self.max
        }
        fn set_duty_cycle(&mut self, duty: u16) -> Result<(), Self::Error> {
            self.duty = duty.min(self.max);
            Ok(())
        }
    }

    #[test]
    fn half_duty_maps_to_half_scale() {
        let mut out = PwmOutput::new(&PwmOutputConfig::default(), FakePwm { max: 1000, duty: 0 });
        out.apply(PwmDuty { duty: 0.5 }, 0).unwrap();
        assert_eq!(out.pwm.duty, 500);
    }

    #[test]
    fn over_range_is_clamped_to_full_scale() {
        let mut out = PwmOutput::new(&PwmOutputConfig::default(), FakePwm { max: 1000, duty: 0 });
        out.apply(PwmDuty { duty: 2.0 }, 0).unwrap();
        assert_eq!(out.pwm.duty, 1000);
    }

    #[test]
    fn negative_is_clamped_to_zero() {
        let mut out = PwmOutput::new(
            &PwmOutputConfig::default(),
            FakePwm {
                max: 1000,
                duty: 500,
            },
        );
        out.apply(PwmDuty { duty: -1.0 }, 0).unwrap();
        assert_eq!(out.pwm.duty, 0);
    }

    #[test]
    fn configured_max_duty_caps_output() {
        // A driver-configured protective ceiling of 0.6 caps a full-on request.
        let mut out = PwmOutput::new(
            &PwmOutputConfig {
                min_duty: 0.0,
                max_duty: 0.6,
                min_update_interval_ms: 0,
            },
            FakePwm { max: 1000, duty: 0 },
        );
        out.apply(PwmDuty { duty: 1.0 }, 0).unwrap();
        assert_eq!(out.pwm.duty, 600);
    }

    #[test]
    fn min_update_interval_drops_early_updates() {
        let mut out = PwmOutput::new(
            &PwmOutputConfig {
                min_duty: 0.0,
                max_duty: 1.0,
                min_update_interval_ms: 100,
            },
            FakePwm { max: 1000, duty: 0 },
        );
        out.apply(PwmDuty { duty: 0.5 }, 0).unwrap();
        assert_eq!(out.pwm.duty, 500);
        // Only 50 ms later — dropped, duty held.
        out.apply(PwmDuty { duty: 1.0 }, 50).unwrap();
        assert_eq!(out.pwm.duty, 500);
        // After the interval — applied.
        out.apply(PwmDuty { duty: 1.0 }, 100).unwrap();
        assert_eq!(out.pwm.duty, 1000);
    }

    #[test]
    fn init_reaches_zero_even_with_a_minimum_duty() {
        // `min_duty` floors a requested duty; it must not floor de-energizing, or
        // a minimum-duty device would run from bring-up onward with nothing
        // reporting it.
        let mut out = PwmOutput::new(
            &PwmOutputConfig {
                min_duty: 0.2,
                max_duty: 1.0,
                min_update_interval_ms: 0,
            },
            FakePwm {
                max: 1000,
                duty: 999,
            },
        );
        out.init().unwrap();
        assert_eq!(
            out.pwm.duty, 0,
            "init must reach 0, not clamp up to min_duty"
        );
    }

    #[test]
    fn init_drives_zero() {
        let mut out = PwmOutput::new(
            &PwmOutputConfig::default(),
            FakePwm {
                max: 1000,
                duty: 999,
            },
        );
        out.init().unwrap();
        assert_eq!(out.pwm.duty, 0);
    }

    #[test]
    fn swapped_bounds_are_normalized_not_panicking() {
        // A mis-ordered config (min > max) must not panic f32::clamp; the driver
        // normalizes the bounds to [0.2, 0.8].
        let mut out = PwmOutput::new(
            &PwmOutputConfig {
                min_duty: 0.8,
                max_duty: 0.2,
                min_update_interval_ms: 0,
            },
            FakePwm { max: 1000, duty: 0 },
        );
        out.apply(PwmDuty { duty: 1.0 }, 0).unwrap();
        assert_eq!(out.pwm.duty, 800, "clamped to normalized max");
        out.apply(PwmDuty { duty: 0.0 }, 0).unwrap();
        assert_eq!(out.pwm.duty, 200, "clamped to normalized min");
    }
}
