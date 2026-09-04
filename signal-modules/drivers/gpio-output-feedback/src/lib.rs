//! Hybrid digital output driver with intrinsic feedback.
//!
//! Drives a relay/output on the `out` pin (delegating to [`gpio_output_driver`]
//! for the command path and its protective limits) and reads the device's real
//! state from a separate `feedback` input pin. The status is a genuine read of
//! an independent line — never inferred from the command write (SDS A1 / OUT-09):
//! `apply` touches only the output; `read_status` touches only the input.

#![cfg_attr(not(test), no_std)]

use embedded_hal::digital::{InputPin, OutputPin};
use gpio_output_driver::{GpioOutput, GpioOutputConfig, GpioOutputError};
use signal_layer_types::DigitalState;

/// Hardware-tier configuration (mirrors [`GpioOutputConfig`], flattened so
/// codegen can assemble it from `config_schema`).
#[derive(Debug, Clone, Copy, Default)]
pub struct GpioOutputFeedbackConfig {
    pub active_low: bool,
    pub min_switch_interval_ms: u64,
    /// When true, the feedback line is active-low (electrical low = asserted),
    /// so `contact` is the inverted pin level. Independent of the output's
    /// `active_low` — the feedback is a separate line.
    pub feedback_active_low: bool,
}

/// Status read back from the device's feedback line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioFeedbackReadings {
    /// The feedback pin's observed level: `true` = the device reports asserted.
    pub contact: bool,
}

/// Error reading the feedback pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioFeedbackError<E> {
    Pin(E),
}

impl<E> From<E> for GpioFeedbackError<E> {
    fn from(e: E) -> Self {
        Self::Pin(e)
    }
}

/// A digital output (`out`) plus an independent feedback input (`feedback`).
pub struct GpioOutputFeedback<O, I> {
    output: GpioOutput<O>,
    feedback: I,
    feedback_active_low: bool,
}

impl<O: OutputPin, I: InputPin> GpioOutputFeedback<O, I> {
    /// Construct from an owned output pin and feedback input pin.
    pub fn new(cfg: &GpioOutputFeedbackConfig, out: O, feedback: I) -> Self {
        let output_cfg = GpioOutputConfig {
            active_low: cfg.active_low,
            min_switch_interval_ms: cfg.min_switch_interval_ms,
        };
        Self {
            output: GpioOutput::new(&output_cfg, out),
            feedback,
            feedback_active_low: cfg.feedback_active_low,
        }
    }

    /// Drive the output to its safe (off) state. Does not read feedback.
    pub fn init(&mut self) -> Result<(), GpioOutputError<O::Error>> {
        self.output.init()
    }

    /// Apply a command — the write path only. Never populates status.
    pub fn apply(
        &mut self,
        cmd: DigitalState,
        now_ms: u64,
    ) -> Result<(), GpioOutputError<O::Error>> {
        self.output.apply(cmd, now_ms)
    }

    /// Read the device's real state from the feedback pin — the only source of
    /// status. Independent of the last command written.
    pub fn read_status(&mut self) -> Result<GpioFeedbackReadings, GpioFeedbackError<I::Error>> {
        // `contact` is logical (asserted); invert the raw level for active-low feedback.
        let contact = self.feedback.is_high()? ^ self.feedback_active_low;
        Ok(GpioFeedbackReadings { contact })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeOut {
        high: bool,
    }
    impl embedded_hal::digital::ErrorType for FakeOut {
        type Error = core::convert::Infallible;
    }
    impl OutputPin for FakeOut {
        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.high = true;
            Ok(())
        }
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.high = false;
            Ok(())
        }
    }

    struct FakeIn {
        high: bool,
    }
    impl embedded_hal::digital::ErrorType for FakeIn {
        type Error = core::convert::Infallible;
    }
    impl InputPin for FakeIn {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.high)
        }
        fn is_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.high)
        }
    }

    #[test]
    fn read_status_reports_feedback_not_the_write() {
        // Feedback pin reads HIGH even though OFF was commanded — proving status
        // is the real read, not the last command (no inference from the write).
        let mut d = GpioOutputFeedback::new(
            &GpioOutputFeedbackConfig::default(),
            FakeOut { high: false },
            FakeIn { high: true },
        );
        d.apply(DigitalState { on: false }, 0).unwrap();
        assert!(
            d.read_status().unwrap().contact,
            "status must come from the feedback pin, not the command"
        );
    }

    #[test]
    fn feedback_active_low_inverts_contact() {
        // Active-low feedback: an electrical LOW means the device is asserted.
        let mut d = GpioOutputFeedback::new(
            &GpioOutputFeedbackConfig {
                feedback_active_low: true,
                ..Default::default()
            },
            FakeOut { high: false },
            FakeIn { high: false },
        );
        assert!(
            d.read_status().unwrap().contact,
            "active-low feedback: low level → asserted (contact true)"
        );
    }

    #[test]
    fn read_status_low_feedback() {
        let mut d = GpioOutputFeedback::new(
            &GpioOutputFeedbackConfig::default(),
            FakeOut { high: false },
            FakeIn { high: false },
        );
        d.apply(DigitalState { on: true }, 0).unwrap();
        assert!(
            !d.read_status().unwrap().contact,
            "feedback low → contact false, regardless of the ON command"
        );
    }
}
