//! Fan-curve feed-forward step — maps an input reading to a PWM duty.
//!
//! A linear transfer function from `[in_min, in_max]` onto `[out_min, out_max]`,
//! clamped to the output range: the continuous counterpart of the digital
//! `hysteresis` controller, for driving a PWM Outlet (e.g. a temperature →
//! fan-speed curve). Feed-forward only — no PID, no fixed-dt control claim.

#![cfg_attr(not(test), no_std)]

use signal_layer_core::ProcessingStep;
use signal_layer_types::PwmDuty;

pub struct FanCurveConfig {
    /// Input value mapped to `out_min`.
    pub in_min: f32,
    /// Input value mapped to `out_max`.
    pub in_max: f32,
    /// Duty at `in_min` (fraction of full scale).
    pub out_min: f32,
    /// Duty at `in_max` (fraction of full scale).
    pub out_max: f32,
}

pub struct FanCurveState {
    in_min: f32,
    in_max: f32,
    out_min: f32,
    out_max: f32,
}

impl FanCurveState {
    #[must_use]
    pub fn new(cfg: FanCurveConfig) -> Self {
        Self {
            in_min: cfg.in_min,
            in_max: cfg.in_max,
            out_min: cfg.out_min,
            out_max: cfg.out_max,
        }
    }
}

impl ProcessingStep for FanCurveState {
    type Input = f32;
    type Output = PwmDuty;

    fn step(&mut self, value: f32) -> Option<PwmDuty> {
        // Normalise the input to 0..=1 across the input span, guarding a zero
        // span (in_min == in_max) which would divide by zero.
        let span = self.in_max - self.in_min;
        let t = if span == 0.0 {
            if value >= self.in_max { 1.0 } else { 0.0 }
        } else {
            ((value - self.in_min) / span).clamp(0.0, 1.0)
        };
        let duty = self.out_min + t * (self.out_max - self.out_min);
        Some(PwmDuty { duty })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve() -> FanCurveState {
        // 20 °C → 0% ramping to 40 °C → 100%.
        FanCurveState::new(FanCurveConfig {
            in_min: 20.0,
            in_max: 40.0,
            out_min: 0.0,
            out_max: 1.0,
        })
    }

    #[test]
    fn maps_midpoint_to_half_duty() {
        assert_eq!(curve().step(30.0), Some(PwmDuty { duty: 0.5 }));
    }

    #[test]
    fn clamps_below_and_above_the_input_span() {
        assert_eq!(curve().step(10.0), Some(PwmDuty { duty: 0.0 }));
        assert_eq!(curve().step(50.0), Some(PwmDuty { duty: 1.0 }));
    }

    #[test]
    fn respects_output_bounds() {
        let mut c = FanCurveState::new(FanCurveConfig {
            in_min: 0.0,
            in_max: 10.0,
            out_min: 0.3,
            out_max: 0.8,
        });
        assert_eq!(c.step(0.0), Some(PwmDuty { duty: 0.3 }));
        assert_eq!(c.step(10.0), Some(PwmDuty { duty: 0.8 }));
    }

    #[test]
    fn zero_span_is_a_step_at_the_threshold() {
        let mut c = FanCurveState::new(FanCurveConfig {
            in_min: 25.0,
            in_max: 25.0,
            out_min: 0.0,
            out_max: 1.0,
        });
        assert_eq!(c.step(24.0), Some(PwmDuty { duty: 0.0 }));
        assert_eq!(c.step(25.0), Some(PwmDuty { duty: 1.0 }));
    }
}
