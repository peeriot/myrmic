//! Generic digital on/off GPIO output driver (relay, LED, valve, …).
//!
//! The write-side counterpart of a sensor driver: instead of `sample()`ing a
//! bus into readings, it `apply()`s a [`DigitalState`] onto a single owned
//! output pin. The pin is generic over [`embedded_hal::digital::OutputPin`], so
//! the same driver serves any chip whose GPIO implements that trait.

#![cfg_attr(not(test), no_std)]

use embedded_hal::digital::OutputPin;
use signal_layer_types::DigitalState;

/// Hardware-tier configuration for a GPIO output.
#[derive(Debug, Clone, Copy, Default)]
pub struct GpioOutputConfig {
    /// When true the pin is driven **low** to assert (active-low wiring, common
    /// for relay boards); when false, asserting drives the pin high.
    pub active_low: bool,
    /// Protective floor: the minimum time (ms) between physical state changes.
    /// A command that would switch the output sooner than this is dropped, so a
    /// relay is never toggled faster than its contacts allow — enforced here,
    /// independent of any pipeline config. `0` disables the limit.
    pub min_switch_interval_ms: u64,
}

/// Error type — a GPIO write failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioOutputError<E> {
    Pin(E),
}

impl<E> From<E> for GpioOutputError<E> {
    fn from(e: E) -> Self {
        Self::Pin(e)
    }
}

/// A digital on/off output bound to a single GPIO pin.
pub struct GpioOutput<P> {
    pin: P,
    active_low: bool,
    min_switch_interval_ms: u64,
    /// Last applied logical state (`None` until the first drive).
    state_on: Option<bool>,
    /// Timestamp (ms) of the last physical switch (`None` until the first).
    last_switch_ms: Option<u64>,
}

impl<P: OutputPin> GpioOutput<P> {
    /// Construct the driver around an owned output pin. Does not drive the pin —
    /// call [`init`](Self::init) to establish the safe (off) state first.
    pub fn new(cfg: &GpioOutputConfig, pin: P) -> Self {
        Self {
            pin,
            active_low: cfg.active_low,
            min_switch_interval_ms: cfg.min_switch_interval_ms,
            state_on: None,
            last_switch_ms: None,
        }
    }

    fn drive(&mut self, on: bool) -> Result<(), GpioOutputError<P::Error>> {
        // active_low inverts the electrical level: asserting (on) drives low.
        let level_high = on ^ self.active_low;
        if level_high {
            self.pin.set_high()?;
        } else {
            self.pin.set_low()?;
        }
        Ok(())
    }

    /// Drive the output to its deasserted (off) state, unconditionally (the
    /// protective floor does not apply to bring-up). Idempotent.
    pub fn init(&mut self) -> Result<(), GpioOutputError<P::Error>> {
        self.drive(false)?;
        self.state_on = Some(false);
        self.last_switch_ms = None;
        Ok(())
    }

    /// Apply a relay command at time `now_ms`, honouring active-low wiring.
    ///
    /// Idempotent (re-commanding the current state is a no-op) and rate-limited:
    /// a command that would switch the output within `min_switch_interval_ms` of
    /// the last switch is dropped — the non-negotiable protective floor.
    pub fn apply(
        &mut self,
        cmd: DigitalState,
        now_ms: u64,
    ) -> Result<(), GpioOutputError<P::Error>> {
        if self.state_on == Some(cmd.on) {
            return Ok(()); // already in the commanded state
        }
        if let Some(last) = self.last_switch_ms
            && now_ms.saturating_sub(last) < self.min_switch_interval_ms
        {
            return Ok(()); // too soon — hold the current state
        }
        self.drive(cmd.on)?;
        self.state_on = Some(cmd.on);
        self.last_switch_ms = Some(now_ms);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal fake pin recording its last driven level.
    struct FakePin {
        high: bool,
    }

    impl embedded_hal::digital::ErrorType for FakePin {
        type Error = core::convert::Infallible;
    }

    impl OutputPin for FakePin {
        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.high = true;
            Ok(())
        }
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.high = false;
            Ok(())
        }
    }

    #[test]
    fn active_high_apply_maps_on_to_high() {
        let mut out = GpioOutput::new(&GpioOutputConfig::default(), FakePin { high: false });
        out.apply(DigitalState { on: true }, 0).unwrap();
        assert!(out.pin.high);
        out.apply(DigitalState { on: false }, 1000).unwrap();
        assert!(!out.pin.high);
    }

    #[test]
    fn active_low_apply_inverts_level() {
        let mut out = GpioOutput::new(
            &GpioOutputConfig {
                active_low: true,
                ..Default::default()
            },
            FakePin { high: false },
        );
        out.apply(DigitalState { on: true }, 0).unwrap();
        assert!(!out.pin.high, "on should drive low when active_low");
        out.apply(DigitalState { on: false }, 1000).unwrap();
        assert!(out.pin.high, "off should drive high when active_low");
    }

    #[test]
    fn init_drives_off() {
        // active-high: off = low
        let mut out = GpioOutput::new(&GpioOutputConfig::default(), FakePin { high: true });
        out.init().unwrap();
        assert!(!out.pin.high);
    }

    #[test]
    fn min_switch_interval_drops_early_switches() {
        let mut out = GpioOutput::new(
            &GpioOutputConfig {
                active_low: false,
                min_switch_interval_ms: 1000,
            },
            FakePin { high: false },
        );
        out.init().unwrap();
        // First switch at t=0 goes through.
        out.apply(DigitalState { on: true }, 0).unwrap();
        assert!(out.pin.high);
        // A switch-back only 500 ms later is dropped (floor is 1000 ms).
        out.apply(DigitalState { on: false }, 500).unwrap();
        assert!(out.pin.high, "early switch must be held off");
        // After the interval elapses, it goes through.
        out.apply(DigitalState { on: false }, 1000).unwrap();
        assert!(!out.pin.high);
    }

    #[test]
    fn re_commanding_current_state_is_a_noop() {
        let mut out = GpioOutput::new(
            &GpioOutputConfig {
                active_low: false,
                min_switch_interval_ms: 1000,
            },
            FakePin { high: false },
        );
        out.apply(DigitalState { on: true }, 0).unwrap();
        // Re-commanding ON is idempotent and does not count as a switch, so a
        // later real change is not blocked by the floor relative to this no-op.
        out.apply(DigitalState { on: true }, 100).unwrap();
        out.apply(DigitalState { on: false }, 1000).unwrap();
        assert!(!out.pin.high);
    }
}
