use core::ffi::c_int;

use crate::{ApiResult, RawBuf, error::ErrorCode};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "logging")]
    unsafe extern "C" {
        /// Requests the host to log the string stored in the provided buffer on the provided level
        ///
        /// # Arguments
        /// - buffer: pointer to a buffer containing the string which is to be logged
        /// - length: the number of bytes in the buffer that the host should read
        /// - ``log_level``: the level to log on (0 is TRACE, 4 is ERROR)
        ///
        /// # Returns
        /// - [`crate::SUCCESS`] on success
        /// - [`crate::GENERIC_ERROR`] on error
        pub(super) fn log_host(buffer: *const u8, length: c_int, log_level: c_int) -> i32;
    }
}

/// Severity of a log line, mirroring the host's log levels.
pub enum LogLevel {
    /// Finest-grained tracing detail.
    Trace,
    /// Diagnostic detail for development.
    Debug,
    /// Normal operational messages.
    Info,
    /// Something unexpected the cell can proceed through.
    Warn,
    /// A failure the cell cannot recover from on its own.
    Error,
}

impl From<LogLevel> for i32 {
    fn from(value: LogLevel) -> Self {
        match value {
            LogLevel::Trace => 0,
            LogLevel::Debug => 1,
            LogLevel::Info => 2,
            LogLevel::Warn => 3,
            LogLevel::Error => 4,
        }
    }
}

impl TryFrom<i32> for LogLevel {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, <LogLevel as TryFrom<i32>>::Error> {
        match value {
            0 => Ok(LogLevel::Trace),
            1 => Ok(LogLevel::Debug),
            2 => Ok(LogLevel::Info),
            3 => Ok(LogLevel::Warn),
            4 => Ok(LogLevel::Error),
            _ => Err(()),
        }
    }
}

/// Logs `msg` on the host's logger at `log_level`.
pub fn log(msg: &str, log_level: LogLevel) -> ApiResult<()> {
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::log_host(msg.as_ptr(), msg.len() as c_int, log_level.into()) }.to_result()
}

/// Logs the contents of a [`RawBuf`] at `log_level` — for contexts that must
/// not allocate, such as the panic and OOM handlers.
pub fn log_buffer(buf: &RawBuf, log_level: LogLevel) -> ApiResult<()> {
    // SAFETY: calling the imported function with pointer and length of guest linear memory
    // used for this specific purpose.
    unsafe { c_functions::log_host(buf.ptr, buf.len as i32, log_level.into()) }.to_result()
}

/// Logs a `format!`-style message at trace level via the host.
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        $crate::log(&$crate::format!($($arg)*), $crate::LogLevel::Trace)
    };
}

/// Logs a plain `&str` at trace level, with no formatting or allocation.
pub fn trace_str(msg: &str) -> ApiResult<()> {
    log(msg, LogLevel::Trace)
}
/// Logs a `format!`-style message at debug level via the host.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::log(&$crate::format!($($arg)*), $crate::LogLevel::Debug)
    };
}

/// Logs a plain `&str` at debug level, with no formatting or allocation.
pub fn debug_str(msg: &str) -> ApiResult<()> {
    log(msg, LogLevel::Debug)
}
/// Logs a `format!`-style message at info level via the host.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::log(&$crate::format!($($arg)*), $crate::LogLevel::Info)
    };
}

/// Logs a plain `&str` at info level, with no formatting or allocation.
pub fn info_str(msg: &str) -> ApiResult<()> {
    log(msg, LogLevel::Info)
}

/// Logs a `format!`-style message at warn level via the host.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::log(&$crate::format!($($arg)*), $crate::LogLevel::Warn)
    };
}

/// Logs a plain `&str` at warn level, with no formatting or allocation.
pub fn warn_str(msg: &str) -> ApiResult<()> {
    log(msg, LogLevel::Warn)
}

/// Logs a `format!`-style message at error level via the host.
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::log(&$crate::format!($($arg)*), $crate::LogLevel::Error)
    };
}

/// Logs a plain `&str` at error level, with no formatting or allocation.
pub fn error_str(msg: &str) -> ApiResult<()> {
    log(msg, LogLevel::Error)
}
