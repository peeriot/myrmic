//! Digital GPIO pins on a Linux character device (`/dev/gpiochipN`).
//!
//! Thin newtypes over [`linux_embedded_hal::CdevPin`] so generated pipeline
//! code names shim types only — the backing implementation can change without
//! touching codegen.

use embedded_hal::digital::{ErrorType, InputPin, OutputPin};
use linux_embedded_hal::gpio_cdev::{Chip, LineRequestFlags, errors::Error as GpioError};
use linux_embedded_hal::{CdevPin, CdevPinError};

/// Consumer label recorded against requested lines (visible in `gpioinfo`).
const CONSUMER: &str = "signal-layer";

/// An owned digital output line on a GPIO character device.
pub struct LinuxOutputPin(CdevPin);

impl LinuxOutputPin {
    /// Open `line` on `chip` (e.g. `/dev/gpiochip0`) as an output, driven to
    /// `initial_high` in the same request so the line never floats in between.
    /// Pass the device's deasserted level so an active-low device is not
    /// briefly asserted before the output driver's `init()` runs.
    ///
    /// # Errors
    ///
    /// Fails if the chip cannot be opened or the line is unavailable
    /// (out of range, or already claimed by another consumer).
    pub fn open(chip: &str, line: u32, initial_high: bool) -> Result<Self, GpioError> {
        let mut chip = Chip::new(chip)?;
        let handle = chip.get_line(line)?.request(
            LineRequestFlags::OUTPUT,
            u8::from(initial_high),
            CONSUMER,
        )?;
        Ok(Self(CdevPin::new(handle)?))
    }
}

impl ErrorType for LinuxOutputPin {
    type Error = CdevPinError;
}

impl OutputPin for LinuxOutputPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.0.set_low()
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.0.set_high()
    }
}

/// An owned digital input line on a GPIO character device (e.g. a hybrid
/// output device's feedback contact).
pub struct LinuxInputPin(CdevPin);

impl LinuxInputPin {
    /// Open `line` on `chip` (e.g. `/dev/gpiochip0`) as an input.
    ///
    /// # Errors
    ///
    /// Fails if the chip cannot be opened or the line is unavailable
    /// (out of range, or already claimed by another consumer).
    pub fn open(chip: &str, line: u32) -> Result<Self, GpioError> {
        let mut chip = Chip::new(chip)?;
        let handle = chip
            .get_line(line)?
            .request(LineRequestFlags::INPUT, 0, CONSUMER)?;
        Ok(Self(CdevPin::new(handle)?))
    }
}

impl ErrorType for LinuxInputPin {
    type Error = CdevPinError;
}

impl InputPin for LinuxInputPin {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        self.0.is_high()
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        self.0.is_low()
    }
}
