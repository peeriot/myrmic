//! The SDK for writing Myrmic cells: WebAssembly modules that deploy and
//! self-organize across a swarm of devices.
//!
//! Myrmic runs application logic as *cells* — portable, isolated Wasm units
//! the runtime deploys, supervises, and connects across embedded, mobile, and
//! server targets. This crate is what a cell links against: it turns plain
//! Rust functions into the exports the runtime invokes, and wraps everything
//! the host offers in safe Rust APIs.
//!
//! # A minimal cell
//!
//! A cell is a `no_std` library crate. There is no `main` and no allocator or
//! panic-handler boilerplate — the default `cell-init` feature emits those via
//! [`cell_prelude!`]. Handlers are free functions marked with the attributes
//! below:
//!
#![doc = concat!(
    "```ignore\n",
    include_str!("doc_examples/counter.rs"),
    "```"
)]
//!
//! The [`tests/fixtures/cell-*`](https://github.com/peeriot/myrmic/tree/master/tests/fixtures)
//! crates are complete working cells exercising each feature; they are the
//! best starting templates.
//!
//! # Handlers
//!
//! | Attribute | Export invoked when | Payload |
//! |---|---|---|
//! | [`#[init]`](macro@init) | Once per incarnation: first deploy, and again on every respawn/restart | optional spawn argument |
//! | [`#[cmd]`](macro@cmd) | Another cell [`send`]s the command (or a timer tick / gateway call names it) | any [`Decoder`]; replies go through a [`Callback`] parameter |
//! | [`#[evt]`](macro@evt) | A subscribed event is [`publish`]ed | the event's payload |
//! | [`#[monitor]`](macro@monitor) | A child cell dies (crash, stop, termination, node loss) | [`monitor::CellLost`] |
//!
//! Every handler takes a leading [`Metadata`] (the cell's own [`Sri`] and the
//! sender's) and returns [`Result`]. Payload types derive [`Message`] to pick
//! their wire codec ([`Json`] by default, or any [`Codec`] via
//! `#[codec(...)]`).
//!
//! # What the host offers
//!
//! - **Messaging** — [`send`] commands to a cell, [`publish`] events to
//!   subscribers, reply via [`Callback`].
//! - **Storage** (the *datalayer*, module [`db`]) — typed handles
//!   [`Kv`](db::tree::Kv), [`Table`](db::table::Table),
//!   [`State`](db::state::State), plus blob, time-series, and semantic
//!   stores. Private db state survives restarts and respawns.
//! - **Spawning & supervision** — [`declare!`] a child class, spawn it with
//!   [`ClassHandle`], get [`monitor`](macro@monitor) callbacks when it dies,
//!   [`terminate_cell`] / [`stop_self`] to tear down.
//! - **Timers** — [`delay`], [`interval`], [`interval_at`] schedule future
//!   invocations of a named command export.
//! - **Time** — [`now`] (swarm-synchronised wall clock), [`uptime`]
//!   (monotonic), [`wait`].
//! - **Logging** — [`trace!`] … [`error!`] macros and their `_str` variants.
//! - **Signal layer** — [`tap`](mod@tap)s onto host-side signals, [`gpio`]
//!   pins, [`ble`] centrals/peripherals.
//! - **Bridges** — [`import!`](macro@import) generates typed clients for
//!   HTTP APIs and MQTT brokers from YAML specs.
//! - **Gateway** — [`gateway`] mounts blob scopes as static assets and routes
//!   HTTP to commands.
//!
//! # Execution model
//!
//! Cells are single-threaded: the runtime invokes one export at a time, so
//! handlers never race each other. A panic is logged with its location and
//! traps the module; the parent (if any) hears about it through its
//! [`monitor`](macro@monitor) handler. Durable state belongs in the
//! datalayer — volatile resources like timers die with the incarnation and
//! are re-established in [`#[init]`](macro@init).
//!
//! # Feature flags
//!
//! | Feature | Default | Enables |
//! |---|---|---|
//! | `alloc` | yes | heap, codecs, everything payload-shaped |
//! | `cells` | yes | messaging, spawning, timers, handler attributes |
//! | `db` | yes | the datalayer APIs |
//! | `cell-init` | yes | the allocator/panic-handler prelude a deployable cell needs |
//! | `eio` | no | `embedded-io` codecs for the shared wire types |
//! | `types-web` | no | `myrmic-common`'s web wire types, for a build that takes that crate without its defaults |
//!
//! The default set is the only configuration that builds today: without
//! `alloc` the mandatory `serde_json` dependency has no allocator, and
//! `myrmic-common` is taken with its own defaults, so the shared `cells`,
//! `db` and `types-web` items - [`types::web`] among them - are present no
//! matter what is selected above.
//!
//! # Further reading
//!
//! The [Myrmic book](https://book.myrmic.intra/) carries the quickstart,
//! tutorials (observability, BLE), and architecture chapters; this crate's
//! docs are the API reference.

#![no_std]
#![cfg_attr(
    all(feature = "alloc", target_arch = "wasm32"),
    feature(alloc_error_handler)
)]
#![warn(missing_docs)]
#![allow(clippy::cast_possible_truncation)] // using the crate just from Wasm -> no need to worry about casting to i32
#![allow(clippy::cast_possible_wrap)] // using the crate just from Wasm -> no need to worry about casting to i32
#![allow(clippy::pedantic)] // temporary: disable pedantic checks for sdk crate
#![allow(clippy::unsafe_derive_deserialize)] // temporary: accepted in current sdk types
#![allow(clippy::new_without_default)] // temporary: whitelist pedantic lint noise in sdk API surface
#![allow(clippy::must_use_candidate)] // temporary: whitelist pedantic lint noise in sdk API surface
#![allow(clippy::missing_errors_doc)] // temporary: whitelist pedantic lint noise in sdk API docs

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "alloc")]
pub use alloc::{format, string::String, vec, vec::Vec};

/// The raw payload buffer type the [`Decoder`]/[`Encoder`] traits work in terms of.
#[cfg(feature = "alloc")]
pub type Bytes = Vec<u8>;

#[cfg(feature = "alloc")]
pub use codec::{Codec, Decoder, Encoder, Json, Postcard, Void};

#[cfg(feature = "alloc")]
pub use serde_json::Value as JsonValue;

#[cfg(feature = "alloc")]
mod allocation;
#[cfg(feature = "alloc")]
mod codec;
mod error;
mod host_functions;
#[cfg(feature = "cells")]
mod messages;
#[cfg(feature = "cells")]
mod messaging;
mod metadata;
mod panic_handlers;

#[cfg(feature = "cells")]
pub use messages::{Callback, Handler};

#[cfg(feature = "cells")]
pub use messaging::{publish, send};

pub use metadata::{Metadata, Sri};

#[cfg(feature = "cells")]
mod traits;

pub use myrmic_common::signal_layer;
pub use myrmic_common::types;

#[cfg(feature = "cells")]
pub use myrmic_common::cells::{Command, Event, EventPublishRequest};

/// Everything a supervising parent needs: the [`monitor`](macro@crate::monitor)
/// handler's payload types and [`stop_self`] for escalation.
///
/// ```ignore
/// use myrmic_sdk::monitor::{CellLost, LostReason};
///
/// #[myrmic_sdk::monitor]
/// fn lost(md: myrmic_sdk::Metadata, l: CellLost) -> myrmic_sdk::Result<()> {
///     // respawn / escalate / ignore
///     Ok(())
/// }
/// ```
pub mod monitor {
    pub use myrmic_common::cells::{CellLost, LostReason};

    pub use crate::host_functions::stop_self;

    use crate::{Decoder, Postcard, Result};

    impl Decoder for CellLost {
        fn from_bytes(bytes: crate::Bytes) -> Result<Self> {
            <Postcard as crate::Codec>::decode(&bytes)
        }
    }
}

#[cfg(feature = "cells")]
pub use host_functions::{
    ClassHandle, ClassRef, CommandError, InMemory, SpawnBuilder, SpawnError, SpawnRequest,
    TerminateError, TimerHandle, delay, interval, interval_at, publish_event, spawn_cell,
    stop_self, terminate_cell,
};

/// Declares a reference to a child cell class by name, returning a
/// [`ClassHandle`] to spawn it with.
///
/// The name is resolved to the child's content hash at deploy time and patched
/// into the module; the reference is stable across redeploys and independent of
/// how the child class is registered. Bind it once and reuse it:
///
/// ```
/// # fn demo(i: u32) -> Result<(), myrmic_sdk::SpawnError> {
/// const CHILD: myrmic_sdk::ClassHandle = myrmic_sdk::declare!("child");
/// CHILD.new().name(format!("child-{i}")).spawn()?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "cells")]
#[macro_export]
macro_rules! declare {
    ($name:literal) => {{
        #[used]
        static __SPAWN_REF: $crate::__private::SpawnRef<{ $name.len() }> =
            $crate::__private::SpawnRef::new($name);
        $crate::ClassHandle::from_hash_ref(__SPAWN_REF.hash_ref())
    }};
}

/// Support items referenced by exported macros; not a stable API.
#[doc(hidden)]
#[cfg(feature = "cells")]
pub mod __private {
    pub use myrmic_common::cells::spawn_ref::SpawnRef;
}

#[cfg(feature = "cells")]
pub use traits::CellEvent;

#[cfg(feature = "cells")]
pub use host_functions::ble;
#[cfg(feature = "cells")]
pub use host_functions::ble::{
    Address, Advertisement, Characteristic, DiscoveredDevice, DiscoveryFilter, ManufacturerData,
    NotifyError, ReadError, ScanMode, Service, ServiceData, Uuid, WriteError, mac_addr_pub,
    mac_addr_rand, uuid128,
};
#[cfg(all(feature = "alloc", feature = "db"))]
pub use host_functions::db;
#[cfg(all(feature = "cells", feature = "db"))]
pub use host_functions::gateway;
pub use host_functions::{
    LogLevel, Outlet, RawBuf, Tap, TapKind, debug_str, error_str, get_arguments, gpio, info_str,
    list_entry, list_len, log, log_buffer, now, outlet, report_error, tap, trace_str, uptime, wait,
    warn_str,
};
pub use signal_layer_types::WireType;

pub use error::{ApiError, ApiResult, Result};
pub use myrmic_common::types::error::*;

/// Re-exported so that `import_cell!`-generated types can derive `Serialize`/`Deserialize`
/// without the consumer crate needing a direct `serde` dependency.
pub use serde;

/// The handler-export attributes, usable as `#[myrmic_sdk::cmd]` etc.
pub use myrmic_sdk_macros::{cmd, evt, import, init, monitor};

/// Derives for the [`Codec`]-backed `Decoder`/`Encoder`
/// impls, usable as `#[derive(myrmic_sdk::Message)]`.
pub use myrmic_sdk_macros::Message;

#[cfg(feature = "alloc")]
pub use allocation::__DefaultHeap;

#[doc(hidden)]
pub use panic_handlers::{__DEFAULT_HEAP_SIZE, __parse_usize};

// The prelude emits a `#[panic_handler]` and the wasm allocator, which only
// compile for the wasm32 cell target.
#[cfg(all(feature = "cell-init", target_arch = "wasm32"))]
cell_prelude!();

// Re-export so the macros can refer to embedded-alloc via `$crate::...`
// and the *consumer* crate doesn't need to depend on the deps of the sdk crate.
#[doc(hidden)]
pub mod __reexports {
    pub use critical_section;
    #[cfg(feature = "alloc")]
    pub use embedded_alloc;
    pub use embedded_hal;
    pub use spin;
}

/// Re-exports the `import!` macro's generated code depends on. Not public API.
#[cfg(feature = "alloc")]
#[doc(hidden)]
pub mod codegen;
