//! Time host functions

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use core::pin::Pin;
use core::time::Duration;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use myrmic_sdk::{EAGAIN, GENERIC_ERROR, SUCCESS};
use postcard::experimental::max_size::MaxSize;
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::NativeSymbol;

use crate::Error;
use crate::async_request::{Request, send_request_and_wait};
use crate::macros::{host_function, host_function_decl};

/// Wall-clock source injected by the firmware: time since `UNIX_EPOCH`, or
/// `None` while the device's clock has not synced with the swarm.
pub(crate) type WallClock = fn() -> Option<Duration>;

static WALL_CLOCK: Mutex<CriticalSectionRawMutex, Cell<Option<WallClock>>> =
    Mutex::new(Cell::new(None));

pub(crate) fn init(clock: WallClock) {
    WALL_CLOCK.lock(|cell| cell.set(Some(clock)));
}

/// Sets up the time imports
#[expect(
    clippy::box_collection,
    reason = "Need to be able to pin from the beginning of the declaration"
)]
pub(crate) fn setup() -> Result<Pin<Box<Vec<NativeSymbol>>>, Error> {
    let native_symbols = Box::pin(vec![
        host_function_decl!(wait_host, c"(*~)i"), // (ptr + len) -> i32
        host_function_decl!(now_host, c"(*~)i"),  // (ptr + len) -> i32
        host_function_decl!(uptime_host, c"(*~)i"), // (ptr + len) -> i32
    ]);

    // safety: C FFI
    let success = unsafe {
        sys::wasm_runtime_register_natives(
            c"time".as_ptr(),
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
fn wait_host(buffer: *const u8, length: i32) -> i32 {
    if buffer.is_null() {
        log::info!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "WAMR host function: WASM i32 args reinterpreted as pointer/size"
    )]
    // safety: we already checked that buffer is non-null
    let slice = unsafe { core::slice::from_raw_parts(buffer, length as usize) };

    let Ok(wait_duration) = postcard::from_bytes::<Duration>(slice) else {
        return GENERIC_ERROR;
    };

    send_request_and_wait(Request::TimeWait(wait_duration));

    SUCCESS
}

#[host_function]
fn now_host(buffer: *mut u8, length: i32) -> i32 {
    // An uninjected clock reports the same way as an unsynced one: no wall
    // time yet, try again.
    let Some(now) = WALL_CLOCK.lock(Cell::get).and_then(|clock| clock()) else {
        return EAGAIN;
    };
    write_duration(buffer, length, now)
}

#[host_function]
fn uptime_host(buffer: *mut u8, length: i32) -> i32 {
    let uptime = Duration::from_micros(embassy_time::Instant::now().as_micros());
    write_duration(buffer, length, uptime)
}

/// Serializes `duration` into the guest buffer, returning the written byte
/// count or an error code.
fn write_duration(buffer: *mut u8, length: i32, duration: Duration) -> i32 {
    if buffer.is_null() {
        log::info!("buffer pointer is null");
        return GENERIC_ERROR;
    }

    let Ok(max_length) = usize::try_from(length) else {
        return GENERIC_ERROR;
    };

    let mut buf = [0u8; Duration::POSTCARD_MAX_SIZE];
    let Ok(encoded) = postcard::to_slice(&duration, &mut buf) else {
        return GENERIC_ERROR;
    };
    let n_bytes = encoded.len();

    if n_bytes > max_length {
        log::error!("buffer too small");
        return GENERIC_ERROR;
    }

    // safety: we already checked that buffer is non-null and n_bytes <= max_length
    let dest = unsafe { core::slice::from_raw_parts_mut(buffer, n_bytes) };
    dest.copy_from_slice(encoded);

    n_bytes
        .try_into()
        .expect("n_bytes is bounded by Duration::POSTCARD_MAX_SIZE")
}
