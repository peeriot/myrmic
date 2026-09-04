//! WASM Imports

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::pin::Pin;

use wamr_rust_sdk::sys::NativeSymbol;

use crate::Error;

mod arguments;
#[cfg(feature = "ble")]
mod ble;
mod cell;
mod db;
mod error;
pub(crate) mod gpio;
mod logging;
pub(crate) mod outlet;
#[cfg(feature = "wdt-selftest")]
pub(crate) mod selftest;
pub(crate) mod tap;
pub(crate) mod time;

/// WASM Imports
// `allow`, not `expect`: the lint fires on the fields, so a struct-level
// expectation is never marked fulfilled.
#[allow(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
#[derive(Debug)]
pub(crate) struct Imports {
    pub _arguments: Pin<Box<Vec<NativeSymbol>>>,
    #[cfg(feature = "ble")]
    pub _ble: Pin<Box<Vec<NativeSymbol>>>,
    pub _cell: Pin<Box<Vec<NativeSymbol>>>,
    pub _db: Pin<Box<Vec<NativeSymbol>>>,
    pub _error: Pin<Box<Vec<NativeSymbol>>>,
    pub _gpio: Pin<Box<Vec<NativeSymbol>>>,
    pub _logging: Pin<Box<Vec<NativeSymbol>>>,
    pub _outlet: Pin<Box<Vec<NativeSymbol>>>,
    #[cfg(feature = "wdt-selftest")]
    pub _selftest: Pin<Box<Vec<NativeSymbol>>>,
    pub _tap: Pin<Box<Vec<NativeSymbol>>>,
    pub _time: Pin<Box<Vec<NativeSymbol>>>,
}

/// Sets up the WASM imports
pub(crate) fn setup() -> Result<Imports, Error> {
    Ok(Imports {
        _arguments: arguments::setup()?,
        #[cfg(feature = "ble")]
        _ble: ble::setup()?,
        _cell: cell::setup()?,
        _db: db::setup()?,
        _error: error::setup()?,
        _gpio: gpio::setup()?,
        _logging: logging::setup()?,
        _outlet: outlet::setup()?,
        #[cfg(feature = "wdt-selftest")]
        _selftest: selftest::setup()?,
        _tap: tap::setup()?,
        _time: time::setup()?,
    })
}
