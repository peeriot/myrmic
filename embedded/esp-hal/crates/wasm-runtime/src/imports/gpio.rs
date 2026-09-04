//! GPIO host functions

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};
use core::pin::Pin;

use esp_hal::gpio::Flex;
use myrmic_sdk::{EINVAL, GENERIC_ERROR, SUCCESS};
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::Error;
use crate::async_request::gpio::Gpio;
use crate::async_request::send_request_and_wait;
use crate::macros::{host_function, host_function_decl};

pub(crate) use esp_hal::gpio::Level;

#[cfg(not(any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61")))]
type InnerPins = ();
#[cfg(feature = "esp32c5")]
type InnerPins = [Option<Flex<'static>>; 25];
#[cfg(feature = "esp32c6")]
type InnerPins = [Option<Flex<'static>>; 28];
#[cfg(feature = "esp32c61")]
type InnerPins = [Option<Flex<'static>>; 30];

/// Collection of GPIO pins that can be used both as inputs and outputs
#[derive(Debug)]
pub struct Pins(pub InnerPins);

impl Deref for Pins {
    type Target = InnerPins;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Pins {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Representation of a pin state of change
#[derive(Debug)]
pub enum Edge {
    Rising,
    Falling,
    Any,
}

/// Sets up the GPIO imports
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(is_pin_supported, c"(i)i"), // (i32) -> i32
        host_function_decl!(set, c"(ii)i"),             // (i32, i32) -> i32
        host_function_decl!(read, c"(i)i"),             // (i32) -> i32
        host_function_decl!(wait_for_level, c"(ii)i"),  // (i32, i32) -> i32
        host_function_decl!(wait_for_edge, c"(ii)i"),   // (i32, i32) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"gpio".as_ptr(),
            native_symbols.as_ptr().cast_mut(),
            native_symbols.len() as u32,
        )
    };

    if success {
        Ok(native_symbols)
    } else {
        Err(Error::Import)
    }
}

#[host_function]
fn is_pin_supported(pin: i32) -> i32 {
    if pin < 0 {
        return EINVAL;
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    i32::from(send_request_and_wait(Gpio::IsPinSupported {
        pin: pin as usize,
    }))
}

#[host_function]
fn set(pin: i32, level: i32) -> i32 {
    if pin < 0 {
        return EINVAL;
    }

    let level = match level {
        0 => Level::Low,
        1 => Level::High,
        _ => return EINVAL,
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    if send_request_and_wait(Gpio::Set {
        pin: pin as usize,
        level,
    })
    .is_ok()
    {
        SUCCESS
    } else {
        GENERIC_ERROR
    }
}

#[host_function]
fn read(pin: i32) -> i32 {
    if pin < 0 {
        return EINVAL;
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    if let Ok(level) = send_request_and_wait(Gpio::Read { pin: pin as usize }) {
        match level {
            Level::Low => 0,
            Level::High => 1,
        }
    } else {
        GENERIC_ERROR
    }
}

#[host_function]
fn wait_for_level(pin: i32, level: i32) -> i32 {
    if pin < 0 {
        return EINVAL;
    }

    let level = match level {
        0 => Level::Low,
        1 => Level::High,
        _ => return EINVAL,
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    if send_request_and_wait(Gpio::WaitForLevel {
        pin: pin as usize,
        level,
    })
    .is_ok()
    {
        SUCCESS
    } else {
        GENERIC_ERROR
    }
}

#[host_function]
fn wait_for_edge(pin: i32, edge: i32) -> i32 {
    if pin < 0 {
        return EINVAL;
    }

    let edge = match edge {
        0 => Edge::Rising,
        1 => Edge::Falling,
        2 => Edge::Any,
        _ => return EINVAL,
    };

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    if send_request_and_wait(Gpio::WaitForEdge {
        pin: pin as usize,
        edge,
    })
    .is_ok()
    {
        SUCCESS
    } else {
        GENERIC_ERROR
    }
}

/// Macro that can be used to gain ownership of the GPIO pins for a [`Context`].
///
/// The macro allows to gain partial ownership of the ESP peripherals, letting the Rust borrow
/// checker making sure to gain unique access to GPIOs, while still letting the user use the rest of
/// the fields of the `Peripherals` structure for other purposes.
///
/// This macro handles automatically the support for different hardwares.
/// When the Signal Layer is active, use the codegen-emitted `pipeline_pins!` instead — it
/// excludes the pins claimed by the board manifest so both subsystems can coexist.
#[macro_export]
macro_rules! pins_from_peripherals {
    ($periph:ident) => {{
        // Share only the GPIOs without restrictions
        #[cfg(feature = "esp32c5")]
        let pins = {
            Pins([
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO0)),
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO1)),
                None,
                None,
                None,
                None,
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO6)),
                None,
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO8)),
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO9)),
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO10)),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO23,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO24,
                )),
            ])
        };
        // Share only the GPIOs without restrictions (both for QFN32 and QFN40 packages)
        #[cfg(feature = "esp32c6")]
        let pins = {
            Pins([
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO0)),
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO1)),
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO2)),
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO3)),
                None,
                None,
                None,
                None,
                None,
                None,
                // Only in QFN40
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO10,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO11,
                )),
                // Exposing these pins as flex pins disconnects them from their very useful function of serial over USB.
                // So for now, we don't touch those.
                None,
                None,
                // Only in QFN32
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO14,
                )),
                None,
                None,
                None,
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO18,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO19,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO20,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO21,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO22,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO23,
                )),
                None,
                None,
                None,
                // Only in QFN40
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO27,
                )),
            ])
        };
        // Share only the GPIOs without restrictions
        #[cfg(feature = "esp32c61")]
        let pins = {
            Pins([
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO0)),
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO1)),
                Some($crate::__reexports::esp_hal::gpio::Flex::new($periph.GPIO2)),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO22,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO23,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO24,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO25,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO26,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO27,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO28,
                )),
                Some($crate::__reexports::esp_hal::gpio::Flex::new(
                    $periph.GPIO29,
                )),
            ])
        };
        #[cfg(not(any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61")))]
        let pins = {
            compile_error!("Only ESP32-C5, ESP32-C6, ESP32-C61 are currently supported");
            Pins(())
        };

        pins
    }};
}
