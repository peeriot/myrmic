//! Tiny logging helpers for myrmic.
//!
//! The verbosity is carried on [`crate::args::Ctx`] via `-v` flags.
//!
//! Use the `error!`, `warn!`, `info!`, `debug!`, `trace!` macros:
//! ```ignore
//! use crate::{info, debug};
//! info!(&ctx, "building {}", name);
//! debug!(&ctx, "opts = {:?}", opts);
//! ```

use std::io::IsTerminal as _;

const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    /// ANSI-styled label printed before the message.
    #[rustfmt::skip]
    pub fn label(self) -> &'static str {
        // Styling is only used when stderr is a tty; callers go through
        // [`emit`] which decides based on [`std::io::stderr().is_terminal()`].
        match self {
            Level::Error => "ERROR",
            Level::Warn =>  "WARN ",
            Level::Info =>  "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }

    /// ANSI color code (foreground) for this level.
    pub fn color(self) -> &'static str {
        match self {
            Level::Error => "\x1b[1;31m", // bold red
            Level::Warn => "\x1b[1;33m",  // bold yellow
            Level::Info => "\x1b[1;32m",  // bold green
            Level::Debug => "\x1b[1;34m", // bold blue
            Level::Trace => "\x1b[1;35m", // bold magenta
        }
    }
}

/// Emit a log line to stderr. Does nothing if the level is disabled.
#[rustfmt::skip]
#[allow(clippy::trivially_copy_pass_by_ref)]
pub fn emit(ctx: &crate::args::Ctx, level: Level, args: std::fmt::Arguments<'_>) {
    use std::io::Write as _;

    if !ctx.is_enabled(level) {
        return;
    }

    let stderr = std::io::stderr();
    let color = stderr.is_terminal();
    let mut handle = stderr.lock();
    if color {
        let _ = writeln!(handle, "{}{}{} {}", level.color(), level.label(), RESET, args);
    } else {
        let _ = writeln!(handle, "{} {}", level.label(), args);
    }
}

#[macro_export]
macro_rules! __log_at {
    ($ctx:expr, $level:expr, $($arg:tt)*) => {{
        // Always bind as a reference so callers can pass either `Ctx` or `&Ctx`.
        // If `$ctx` is `Ctx`, `&$ctx` is `&Ctx`.
        // If `$ctx` is `&Ctx`, `&$ctx` is `&&Ctx` — deref coercion handles the rest.
        let ctx: &$crate::args::Ctx = &$ctx;
        let level = $level;
        if ctx.is_enabled(level) {
            $crate::log::emit(ctx, level, ::std::format_args!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! error {
    ($ctx:expr, $($arg:tt)*) => {
        $crate::__log_at!($ctx, $crate::log::Level::Error, $($arg)*)
    };
}

#[macro_export]
macro_rules! warn {
    ($ctx:expr, $($arg:tt)*) => {
        $crate::__log_at!($ctx, $crate::log::Level::Warn, $($arg)*)
    };
}

#[macro_export]
macro_rules! info {
    ($ctx:expr, $($arg:tt)*) => {
        $crate::__log_at!($ctx, $crate::log::Level::Info, $($arg)*)
    };
}

#[macro_export]
macro_rules! debug {
    ($ctx:expr, $($arg:tt)*) => {
        $crate::__log_at!($ctx, $crate::log::Level::Debug, $($arg)*)
    };
}

#[macro_export]
macro_rules! trace {
    ($ctx:expr, $($arg:tt)*) => {
        $crate::__log_at!($ctx, $crate::log::Level::Trace, $($arg)*)
    };
}
