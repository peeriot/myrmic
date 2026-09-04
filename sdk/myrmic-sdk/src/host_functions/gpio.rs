//! GPIO Host functions
//!
//! Provides `GpioX` structs that implement the
//! [`InputPin`](embedded_hal::digital::InputPin) and
//! [`OutputPin`](embedded_hal::digital::OutputPin) traits

/// Macro for implementing GPIOs and their
/// [`InputPin`](embedded_hal::digital::InputPin),
/// [`OutputPin`](embedded_hal::digital::OutputPin) and [`Wait`] (a blocking
/// copy of `embedded_hal_async::digital::Wait`) traits using WASM host
/// functions.
///
/// This implements both the `GpioX` structs and their behaviour as "Input/Output pins". This can be
/// used to easily implement the necessary functions and types for a range of pins.
///
/// # Usage
///
/// Calling the macro in a way like
///
/// ```ignore
/// impl_pin! {
///     4, 6, 7
/// }
/// ```
///
/// would expand and generate structs with names `Gpio4`, `Gpio6` and `Gpio7`. Those structs can
/// then be operated as if they were hardware pins via the embedded-hal(-async) traits.
///
/// ```
/// # fn demo() -> Result<(), embedded_hal::digital::ErrorKind> {
/// use embedded_hal::digital::{InputPin, OutputPin};
/// use myrmic_sdk::gpio::Gpio4;
///
/// let Some(mut gpio_4) = Gpio4::try_get() else {
///     panic!("GPIO 4 is not available on the host hardware");
/// };
///
/// if gpio_4.is_low()? {
///     // react to the low pin
/// }
/// if gpio_4.is_high()? {
///     // react to the high pin
/// }
///
/// gpio_4.set_low()?;
/// gpio_4.set_high()?;
/// # Ok(())
/// # }
/// ```
///
/// Also [`Wait`] is implemented for those structs, so that one can make blocking requests in a
/// similar way as using the async `embedded_hal_async::digital::Wait` traits.
///
/// ```
/// # fn demo() -> Result<(), embedded_hal::digital::ErrorKind> {
/// use myrmic_sdk::gpio::{Gpio4, Wait};
///
/// let Some(mut gpio_4) = Gpio4::try_get() else {
///     panic!("GPIO 4 is not available on the host hardware");
/// };
///
/// loop {
///     gpio_4.wait_for_any_edge()?;
///     // the pin state changed
/// }
/// # }
/// ```
macro_rules! impl_pin {
    ($($pin:expr),+ $(,)?) => {
        $(
            paste::paste!{
                #[doc = concat!("GPIO pin ", stringify!($pin), ", driven through the host's GPIO imports.")]
                #[derive(Default)]
                pub struct [<Gpio $pin>];

                impl [<Gpio $pin>] {
                    const PIN_ID: i32 = $pin;

                    /// Claims the pin if the host hardware exposes it, `None` otherwise.
                    pub fn try_get() -> Option<Self> {
                        // SAFETY: calling the imported function without handling memory
                        if unsafe { is_pin_supported(Self::PIN_ID) == 1 } {
                            Some(Self)
                        } else {
                            None
                        }
                    }
                }

                impl $crate::__reexports::embedded_hal::digital::ErrorType for [<Gpio $pin>] {
                    type Error = $crate::__reexports::embedded_hal::digital::ErrorKind;
                }

                impl $crate::__reexports::embedded_hal::digital::InputPin for [<Gpio $pin>] {
                    fn is_high(&mut self) -> Result<bool, Self::Error> {
                        // SAFETY: calling the imported function without handling memory
                        unsafe {
                            match read_pin(Self::PIN_ID) {
                                0 => Ok(false),
                                1 => Ok(true),
                                _ => Err($crate::__reexports::embedded_hal::digital::ErrorKind::Other),
                            }
                        }
                    }

                    fn is_low(&mut self) -> Result<bool, Self::Error> {
                        // SAFETY: calling the imported function without handling memory
                        unsafe {
                            match read_pin(Self::PIN_ID) {
                                0 => Ok(true),
                                1 => Ok(false),
                                _ => Err($crate::__reexports::embedded_hal::digital::ErrorKind::Other),
                            }
                        }
                    }
                }

                impl $crate::host_functions::gpio::Wait for [<Gpio $pin>] {
                    fn wait_for_high(&mut self) -> Result<(), Self::Error>{
                        // SAFETY: calling the imported function without handling memory
                        unsafe {
                            if wait_for_level(Self::PIN_ID, 1) == 0 {
                                Ok(())
                            } else {
                                Err($crate::__reexports::embedded_hal::digital::ErrorKind::Other)
                            }
                        }
                    }

                    fn wait_for_low(&mut self) -> Result<(), Self::Error>{
                        // SAFETY: calling the imported function without handling memory
                        unsafe {
                            if wait_for_level(Self::PIN_ID, 0) == 0 {
                                Ok(())
                            } else {
                                Err($crate::__reexports::embedded_hal::digital::ErrorKind::Other)
                            }
                        }
                    }

                    fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error>{
                        // SAFETY: calling the imported function without handling memory
                        unsafe {
                            if wait_for_edge(Self::PIN_ID, 0) == 0 {
                                Ok(())
                            } else {
                                Err($crate::__reexports::embedded_hal::digital::ErrorKind::Other)
                            }
                        }
                    }

                    fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error>{
                        // SAFETY: calling the imported function without handling memory
                        unsafe {
                            if wait_for_edge(Self::PIN_ID, 1) == 0 {
                                Ok(())
                            } else {
                                Err($crate::__reexports::embedded_hal::digital::ErrorKind::Other)
                            }
                        }
                    }

                    fn wait_for_any_edge(&mut self) -> Result<(), Self::Error>{
                        // SAFETY: calling the imported function without handling memory
                        unsafe {
                            if wait_for_edge(Self::PIN_ID, 2) == 0 {
                                Ok(())
                            } else {
                                Err($crate::__reexports::embedded_hal::digital::ErrorKind::Other)
                        }
                    }
                }
                }

                impl $crate::__reexports::embedded_hal::digital::OutputPin for [<Gpio $pin>] {
                    fn set_low(&mut self) -> Result<(), Self::Error> {
                        // SAFETY: calling the imported function without handling memory
                        unsafe {
                            if set(Self::PIN_ID, 0) == 0 {
                                Ok(())
                            } else {
                                Err($crate::__reexports::embedded_hal::digital::ErrorKind::Other)
                            }
                        }
                    }


                    fn set_high(&mut self) -> Result<(), Self::Error> {
                        // SAFETY: calling the imported function without handling memory
                        unsafe {
                            if set(Self::PIN_ID, 1) == 0 {
                                Ok(())
                            } else {
                                Err($crate::__reexports::embedded_hal::digital::ErrorKind::Other)
                            }
                        }
                    }
                }
        })+
    };
}

// Implement all pins, then let the host function return an error whether the pin can't be used or
// whether input/output operations are available
impl_pin! {
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
}

/// Cloned trait from `embedded_hal_async::digital::Wait` but with a blocking implementation
pub trait Wait: embedded_hal::digital::ErrorType {
    /// Wait until the pin is high. If it is already high, return immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if the pin cannot be controlled as an input
    fn wait_for_high(&mut self) -> Result<(), Self::Error>;
    /// Wait until the pin is low. If it is already low, return immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if the pin cannot be controlled as an input
    fn wait_for_low(&mut self) -> Result<(), Self::Error>;
    /// Wait for the pin to undergo a transition from low to high.
    ///
    /// If the pin is already high, this does *not* return immediately, it'll wait for the
    /// pin to go low and then high again.
    ///
    /// # Errors
    ///
    /// Returns an error if the pin cannot be controlled as an input
    fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error>;
    /// Wait for the pin to undergo a transition from high to low.
    ///
    /// If the pin is already low, this does *not* return immediately, it'll wait for the
    /// pin to go high and then low again.
    ///
    /// # Errors
    ///
    /// Returns an error if the pin cannot be controlled as an input
    fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error>;
    /// Wait for the pin to undergo any transition, i.e low to high OR high to low.
    ///
    /// # Errors
    ///
    /// Returns an error if the pin cannot be controlled as an input
    fn wait_for_any_edge(&mut self) -> Result<(), Self::Error>;
}

#[link(wasm_import_module = "gpio")]
unsafe extern "C" {
    /// Returns whether the pin is supported and available for use on the host hardware
    ///
    /// # Arguments
    /// - pin: GPIO pin number
    ///
    /// # Returns
    /// - 0 if unavailable
    /// - 1 if available
    /// - [`crate::EINVAL`] if the passed pin is negative
    fn is_pin_supported(pin: i32) -> i32;
    /// Requests the host to set the selected GPIO pin to the selected level
    ///
    /// # Arguments
    /// - pin: GPIO pin number
    /// - level:
    ///  * `0` - Low
    ///  * `1` - High
    ///
    /// # Returns
    /// - [`crate::SUCCESS`] on success
    /// - [`crate::EINVAL`] if an invalid argument is passed
    /// - [`crate::GENERIC_ERROR`] on error
    fn set(pin: i32, level: i32) -> i32;
    /// Requests the host to read the state of a GPIO pin
    ///
    /// # Arguments
    /// - pin: GPIO pin number
    ///
    /// # Returns
    /// - 0: Low
    /// - 1: High
    /// - [`crate::EINVAL`] if a negative pin number is passed
    /// - [`crate::GENERIC_ERROR`] on error
    #[cfg_attr(target_arch = "wasm32", link_name = "read")]
    fn read_pin(pin: i32) -> i32;
    /// Requests the host to await for the selected GPIO pin to be in the selected level
    ///
    /// # Arguments
    /// - pin: GPIO pin number
    /// - level:
    ///  * `0` - Low
    ///  * `1` - High
    ///
    /// # Returns
    /// - [`crate::SUCCESS`] on success
    /// - [`crate::EINVAL`] if an invalid argument is passed
    /// - [`crate::GENERIC_ERROR`] on error
    fn wait_for_level(pin: i32, level: i32) -> i32;
    /// Requests the host to await for the selected GPIO pin to witness the selected edge
    ///
    /// # Arguments
    /// - pin: GPIO pin number
    /// - edge:
    ///  * `0` - Rising
    ///  * `1` - Falling
    ///  * `2` - Any
    ///
    /// # Returns
    /// - [`crate::SUCCESS`] on success
    /// - [`crate::EINVAL`] if an invalid argument is passed
    /// - [`crate::GENERIC_ERROR`] on error
    fn wait_for_edge(pin: i32, edge: i32) -> i32;
}
