//! Module containing the (safe) functions which can be used to interact with the Wasm host

use core::fmt::Write;
use serde::Serialize;

#[cfg(feature = "alloc")]
pub mod ble;
#[cfg(all(feature = "alloc", feature = "db"))]
pub mod db;
mod errors;

#[cfg(feature = "cells")]
mod cell;
#[cfg(feature = "cells")]
mod in_memory;

#[cfg(all(feature = "cells", feature = "db"))]
pub mod gateway;

// This is public because we can't easily re-export generated APIs
mod arguments;
pub mod gpio;
mod logging;
pub mod outlet;
pub mod tap;
mod time;

pub use arguments::get_arguments;

#[cfg(feature = "cells")]
pub use cell::{
    ClassHandle, ClassRef, CommandError, SpawnBuilder, SpawnError, SpawnRequest, TerminateError,
    TimerHandle, delay, interval, interval_at, spawn_cell, stop_self, terminate_cell,
};

#[cfg(feature = "cells")]
pub use cell::publish_event;
#[cfg(feature = "cells")]
pub(crate) use cell::send_command;
#[cfg(feature = "cells")]
pub use in_memory::InMemory;

pub use errors::report_error;
pub use logging::{LogLevel, debug_str, error_str, info_str, log, log_buffer, trace_str, warn_str};
pub use outlet::Outlet;
pub use tap::{Tap, TapKind, list_entry, list_len};
pub use time::{now, uptime, wait};

/// A fixed-capacity output buffer described by a raw pointer and capacity,
/// written through `core::fmt::Write`; text past the capacity is truncated.
///
/// Lets the panic and OOM handlers format log messages without allocating —
/// pass the result to [`log_buffer`].
pub struct RawBuf {
    ptr: *mut u8,
    cap: usize,
    len: usize,
}

impl RawBuf {
    /// Wraps the `cap`-byte buffer at `ptr`, starting empty.
    ///
    /// The caller must keep `ptr` valid for writes of `cap` bytes for as long
    /// as the `RawBuf` is used.
    pub fn new(ptr: *mut u8, cap: usize) -> Self {
        Self { ptr, cap, len: 0 }
    }
}

#[allow(clippy::cast_sign_loss)]
impl Write for RawBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remaining_cap = self.cap - self.len;
        let copy = bytes.len().min(remaining_cap);
        // SAFETY: `bytes.as_ptr()` is valid for reads of `copy` bytes because
        // `copy <= bytes.len()`. `self.ptr.add(self.len)` points into the writable
        // buffer owned by `RawBuf`, and `copy <= self.cap - self.len`, so the
        // destination range is valid for writes of `copy` bytes. The source slice
        // `bytes` cannot overlap with `RawBuf`'s output buffer here.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr.add(self.len), copy);
        }
        self.len += copy;
        Ok(())
    }
}

#[cfg(feature = "alloc")]
#[allow(dead_code)]
pub(crate) fn call<T>(
    req: &T,
    func: unsafe extern "C" fn(buffer: *const u8, length: core::ffi::c_int) -> core::ffi::c_int,
) -> Result<(), core::ffi::c_int>
where
    T: Serialize + ?Sized,
{
    let bytes = postcard::to_allocvec(&req).map_err(|_err| -120 as core::ffi::c_int)?;

    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    let errno = unsafe { func(bytes.as_ptr(), bytes.len() as i32) };

    match errno {
        0 => Ok(()),
        n => Err(n),
    }
}
