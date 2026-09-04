use core::ffi::c_int;
use core::time::Duration;

use postcard::experimental::max_size::MaxSize;

use crate::error::ErrorCode;
use crate::{ApiError, ApiResult};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "time")]
    unsafe extern "C" {
        /// Requests the host to pause the module for a specified duration of time
        ///
        /// # Arguments
        /// - buffer: pointer to a buffer containing the serialized Duration
        /// - length: the number of bytes in the buffer of the serialized Duration
        ///
        /// # Returns
        /// - [`crate::SUCCESS`] on success
        /// - [`crate::GENERIC_ERROR`] on error
        pub(super) fn wait_host(buffer: *const u8, length: c_int) -> i32;

        /// Requests the host to write the current wall-clock time (elapsed since
        /// `UNIX_EPOCH`, from the host's swarm-synchronised hybrid logical clock)
        /// into the given buffer, serialized as a `Duration`.
        ///
        /// # Arguments
        /// - buffer: pointer to the module memory where the host shall write the serialized Duration
        /// - length: maximal length of the buffer
        ///
        /// # Returns
        /// - n of written bytes on success (non-negative number)
        /// - [`crate::EAGAIN`] while the host's clock has not yet synced with the swarm
        /// - [`crate::GENERIC_ERROR`] on error
        pub(super) fn now_host(buffer: *mut u8, length: c_int) -> i32;

        /// Requests the host to write its current monotonic uptime (elapsed since the
        /// host process started or the device booted) into the given buffer,
        /// serialized as a `Duration`.
        ///
        /// # Arguments
        /// - buffer: pointer to the module memory where the host shall write the serialized Duration
        /// - length: maximal length of the buffer
        ///
        /// # Returns
        /// - n of written bytes on success (non-negative number)
        /// - [`crate::GENERIC_ERROR`] on error
        pub(super) fn uptime_host(buffer: *mut u8, length: c_int) -> i32;
    }
}

/// Requests the host pause the module for `dur`.
pub fn wait(dur: Duration) -> ApiResult<()> {
    let mut buf = [0; Duration::POSTCARD_MAX_SIZE];
    postcard::to_slice(&dur, &mut buf).map_err(|_| ApiError::UnknownErrorCode(0))?;
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::wait_host(buf.as_ptr(), buf.len() as c_int).to_result() }
}

/// Returns the current wall-clock time as a `Duration` since `UNIX_EPOCH`,
/// from the host's swarm-synchronised hybrid logical clock.
///
/// Errs with [`ApiError::NotReady`] on an embedded host whose clock has not
/// yet synced with the swarm; sync completes on the device's first
/// timestamped exchange, normally well before any cell runs.
#[allow(clippy::cast_sign_loss)]
pub fn now() -> ApiResult<Duration> {
    let mut buf = [0; Duration::POSTCARD_MAX_SIZE];
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    let status = unsafe { c_functions::now_host(buf.as_mut_ptr(), buf.len() as c_int) };
    match status {
        n if n >= 0 => {
            postcard::from_bytes(&buf[..n as usize]).map_err(|_| ApiError::UnknownErrorCode(0))
        }
        code => Err(code.into()),
    }
}

/// Returns the host's monotonic uptime: elapsed time since the host process
/// started (edge) or since boot (embedded).
///
/// Strictly monotonic within one host incarnation and independent of clock
/// sync, so it suits interval and deadline arithmetic. It resets on a host
/// restart and is not comparable across nodes — for a wall-clock ordering
/// stamp use [`now`] instead.
#[allow(clippy::cast_sign_loss)]
pub fn uptime() -> ApiResult<Duration> {
    let mut buf = [0; Duration::POSTCARD_MAX_SIZE];
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    let status = unsafe { c_functions::uptime_host(buf.as_mut_ptr(), buf.len() as c_int) };
    match status {
        n if n >= 0 => {
            postcard::from_bytes(&buf[..n as usize]).map_err(|_| ApiError::UnknownErrorCode(0))
        }
        code => Err(code.into()),
    }
}
