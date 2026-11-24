//! Digital GPIO pins on a Linux character device (`/dev/gpiochipN`).
//!
//! Thin newtypes over [`linux_embedded_hal::CdevPin`] so generated pipeline
//! code names shim types only — the backing implementation can change without
//! touching codegen.

use std::time::Duration;

use embedded_hal::digital::{ErrorType, InputPin, OutputPin};
use embedded_hal_async::digital::Wait;
use linux_embedded_hal::gpio_cdev::{Chip, LineRequestFlags, errors::Error as GpioError};
use linux_embedded_hal::{CdevPin, CdevPinError};

/// Consumer label recorded against requested lines (visible in `gpioinfo`).
const CONSUMER: &str = "signal-layer";

/// Sampling period of the polled [`Wait`] implementation.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

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

// ponytail: polls the level every millisecond; switch to gpio-cdev edge events
// if latency or idle CPU matters.
impl Wait for LinuxInputPin {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        wait_for_level(self, true).await
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        wait_for_level(self, false).await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        wait_for_edge(self, true).await
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        wait_for_edge(self, false).await
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        wait_for_change(self).await
    }
}

/// Wait for `pin` to leave the opposite level and then reach `rising`'s level.
async fn wait_for_edge<P>(pin: &mut P, rising: bool) -> Result<(), P::Error>
where
    P: InputPin,
{
    wait_for_level(pin, !rising).await?;

    wait_for_level(pin, rising).await
}

/// Wait for `pin` to leave the level it has now.
async fn wait_for_change<P>(pin: &mut P) -> Result<(), P::Error>
where
    P: InputPin,
{
    let start = pin.is_high()?;

    wait_for_level(pin, !start).await
}

/// Sample `pin` every [`POLL_INTERVAL`] until it reads `high`.
async fn wait_for_level<P>(pin: &mut P, high: bool) -> Result<(), P::Error>
where
    P: InputPin,
{
    while pin.is_high()? != high {
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;

    use super::*;

    #[test]
    fn shim_pins_implement_the_driver_traits() {
        assert_wait::<LinuxInputPin>();
        assert_output_pin::<LinuxOutputPin>();
    }

    #[tokio::test(start_paused = true)]
    async fn level_wait_returns_immediately_when_already_there() {
        let mut pin = FakePin([true].into());

        wait_for_level(&mut pin, true).await.unwrap();

        assert_eq!(pin.0.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn level_wait_polls_until_the_level_arrives() {
        let mut pin = FakePin([false, false, false, true].into());
        let start = tokio::time::Instant::now();

        wait_for_level(&mut pin, true).await.unwrap();

        assert_eq!(start.elapsed(), POLL_INTERVAL * 3);
    }

    #[tokio::test(start_paused = true)]
    async fn falling_edge_needs_a_high_before_the_low() {
        let mut pin = FakePin([false, false, true, false].into());
        let start = tokio::time::Instant::now();

        wait_for_edge(&mut pin, false).await.unwrap();

        assert_eq!(start.elapsed(), POLL_INTERVAL * 2);
    }

    #[tokio::test(start_paused = true)]
    async fn edge_wait_ignores_a_level_that_is_already_there() {
        let mut pin = FakePin([true, true, true, false, true].into());
        let start = tokio::time::Instant::now();

        wait_for_edge(&mut pin, true).await.unwrap();

        assert_eq!(start.elapsed(), POLL_INTERVAL * 3);
    }

    #[tokio::test(start_paused = true)]
    async fn change_wait_leaves_the_start_level_in_either_direction() {
        for (script, ticks) in [
            (&[true, true, true, false][..], 2),
            (&[false, false, true][..], 1),
        ] {
            let mut pin = FakePin(script.iter().copied().collect());
            let start = tokio::time::Instant::now();

            wait_for_change(&mut pin).await.unwrap();

            assert_eq!(start.elapsed(), POLL_INTERVAL * ticks);
        }
    }

    /// Reads the scripted levels in order, then repeats the last one.
    struct FakePin(VecDeque<bool>);

    impl ErrorType for FakePin {
        type Error = Infallible;
    }

    impl InputPin for FakePin {
        fn is_high(&mut self) -> Result<bool, Infallible> {
            Ok(if self.0.len() > 1 {
                self.0.pop_front().unwrap_or_default()
            } else {
                self.0.front().copied().unwrap_or_default()
            })
        }

        fn is_low(&mut self) -> Result<bool, Infallible> {
            self.is_high().map(|high| !high)
        }
    }

    fn assert_wait<T: Wait>() {}

    fn assert_output_pin<T: OutputPin>() {}
}
