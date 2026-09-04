//! Tokio scaffold — Linux tokio equivalents for each runtime hook (D5 table).
//!
//! `LinuxChipBackend` overrides each runtime hook with a one-line delegation
//! into these functions.

use proc_macro2::TokenStream;
use quote::quote;

// ── emit_runtime_imports ─────────────────────────────────────────────────────

/// Tokio `use` items for the generated Linux pipeline crate.
///
/// `IntervalStream` wraps `tokio::time::Interval` as a `Stream` so that
/// the chip-agnostic emitter's `ticker.next().await` pattern compiles.
/// `StreamExt` brings `.next()` into scope.
pub fn emit_runtime_imports() -> TokenStream {
    quote! {
        use tokio::time::{interval, Duration};
        use tokio_stream::{StreamExt as _, wrappers::IntervalStream};
    }
}

// ── emit_task_attribute ──────────────────────────────────────────────────────

/// Linux tokio tasks need no task attribute — returns empty.
pub fn emit_task_attribute() -> TokenStream {
    quote! {}
}

// ── emit_interval ────────────────────────────────────────────────────────────

/// `IntervalStream::new(interval(Duration::from_millis(#ms)))` expression.
///
/// Wraps `tokio::time::Interval` as a `Stream` so the chip-agnostic
/// `ticker.next().await` pattern compiles on both Embassy (Embassy `Ticker`
/// already implements `Stream`) and tokio (`Interval` does not, hence the
/// `IntervalStream` wrapper from `tokio-stream`).
pub fn emit_interval(ms: u64) -> TokenStream {
    let ms_lit = proc_macro2::Literal::u64_suffixed(ms);
    quote! { IntervalStream::new(interval(Duration::from_millis(#ms_lit))) }
}

// ── emit_now_millis ──────────────────────────────────────────────────────────

/// `signal_layer_linux_rt::time::now_millis()` expression — the fenced time
/// seam (SR-15, D6).
pub fn emit_now_millis() -> TokenStream {
    quote! { signal_layer_linux_rt::time::now_millis() }
}

// ── emit_spawn ───────────────────────────────────────────────────────────────

/// `tokio::spawn(#task)` call (no `Spawner` parameter, no `.expect()`).
///
/// The `label` argument is the embassy panic message — ignored on tokio
/// because `async fn` returns a `Future` (no `.expect()` needed).
pub fn emit_spawn(task: &TokenStream, _label: &str) -> TokenStream {
    quote! { tokio::spawn(#task); }
}

// ── emit_tap_handoff ─────────────────────────────────────────────────────────

/// Hand the tap registry to the IPC server via
/// `signal_layer_linux_rt::run_tap_server(...)`.
///
/// The exact wiring is filled in by `LinuxChipBackend` in `linux-codegen`;
/// this stub emits a compile-time marker so the hook is clearly overridden.
pub fn emit_tap_handoff() -> TokenStream {
    quote! {
        // Linux: hand registry to signal_layer_linux_rt::run_tap_server(...)
        // (wired by LinuxChipBackend::emit_tap_handoff in linux-codegen)
    }
}

// ── emit_bus_static ──────────────────────────────────────────────────────────

/// Linux: no `StaticCell` needed — the shim bus is constructed inline.
/// `bus` and `inner` arguments are unused on this platform.
pub fn emit_bus_static(_bus: &TokenStream, _inner: &TokenStream) -> TokenStream {
    quote! {}
}

// ── emit_bus_device_new ──────────────────────────────────────────────────────

/// Linux: clone the shim handle — `#bus.clone()`.
pub fn emit_bus_device_new(bus: &TokenStream) -> TokenStream {
    quote! { #bus.clone() }
}

// ── emit_spi_bus_device_new ──────────────────────────────────────────────────

/// Linux: bind a software-CS device to the shared spidev bus — the CS is the
/// generated `BoardPeripherals` field named by `cs` (a GPIO character-device
/// line; the kernel chip-select is disabled on the bus).
pub fn emit_spi_bus_device_new(bus: &TokenStream, cs: &TokenStream) -> TokenStream {
    quote! { #bus.device(peripherals.#cs) }
}

// ── emit_bus_device_type ─────────────────────────────────────────────────────

/// Linux: `linux_i2c_shim::SharedI2c<linux_i2c_shim::LinuxI2cdev>` — inner
/// type ignored (the shim has a fixed concrete type).
pub fn emit_bus_device_type(_inner: &TokenStream) -> TokenStream {
    quote! { linux_i2c_shim::SharedI2c<linux_i2c_shim::LinuxI2cdev> }
}

// ── emit_spi_bus_device_type ─────────────────────────────────────────────────

/// Linux: `linux_spi_shim::SharedSpiDevice<#inner, #cs>` — the shim's
/// software-CS device over the shared spidev bus.
pub fn emit_spi_bus_device_type(inner: &TokenStream, cs: &TokenStream) -> TokenStream {
    quote! { linux_spi_shim::SharedSpiDevice<#inner, #cs> }
}

// ── emit_bus_init ────────────────────────────────────────────────────────────

/// Linux: no `StaticCell` to initialise — the shim bus is constructed and
/// shared differently. Returns an empty `TokenStream`.
pub fn emit_bus_init(
    _bus_var: &TokenStream,
    _static_ident: &TokenStream,
    _bus_field: &TokenStream,
    _inner: &TokenStream,
) -> TokenStream {
    quote! {}
}

// ── emit_outlet_handoff ──────────────────────────────────────────────────────

/// Linux v1: outlet-bearing pipelines are rejected (SR-19), so this is a
/// no-op stub.
pub fn emit_outlet_handoff() -> TokenStream {
    quote! {}
}
