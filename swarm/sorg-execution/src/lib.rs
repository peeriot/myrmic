//! Implements the functionality of the zenoh sorg execution plugin

mod error;
mod event_loop;
mod mqtt;
mod payload;
mod queryables;
mod spawn;
mod supervision;
mod wasm;

/// The mailbox-native HTTP/MQTT bridge handles.
///
/// Public so the orchestration plugin can spawn them directly by SRI (native bridge
/// cell deploy), instead of routing through the operator/deployment engine.
pub mod bridge {
    pub(crate) mod consumer;
    pub mod http;
    pub mod mqtt;
}

pub use error::{Error, Result};
pub use spawn::spawn;

/// Public re-exports for tests and integration harnesses.
pub mod wasm_tap {
    pub use crate::wasm::{
        CellIdentity, link_outlet_functions, link_tap_functions, release_sl_claim,
    };
}

pub(crate) use event_loop::Event;

/// Baseline captured once, on first access, for the lifetime of this host process.
/// A few milliseconds of drift between actual process start and first access is not
/// significant for an uptime counter.
pub(crate) static PROCESS_START: std::sync::LazyLock<std::time::Instant> =
    std::sync::LazyLock::new(std::time::Instant::now);
