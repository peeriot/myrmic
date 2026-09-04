//! Embassy scaffold — verbatim copies of the token blocks that are currently
//! emitted inline in `pipeline-codegen/src/emit/*.rs`.
//!
//! These functions become the default implementations of the ten runtime hooks
//! on `ChipBackend`, so `Esp32Backend` (and any future Embassy-based backend)
//! writes zero runtime code.

use proc_macro2::TokenStream;
use quote::quote;

// ── emit_runtime_imports ─────────────────────────────────────────────────────

/// Common Embassy `use` items injected at the top of the generated file
/// (`imports.rs` inline block).
pub fn emit_runtime_imports() -> TokenStream {
    quote! {
        use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
        use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
        use embassy_executor::Spawner;
        use embassy_sync::blocking_mutex::raw::NoopRawMutex;
        use embassy_sync::mutex::Mutex;
        use embassy_time::{Duration, Ticker};
        use static_cell::StaticCell;
    }
}

// ── emit_task_attribute ──────────────────────────────────────────────────────

/// The `#[embassy_executor::task]` attribute emitted on source/sink task fns
/// (`source_task.rs` and `sink_task.rs` inline block).
pub fn emit_task_attribute() -> TokenStream {
    quote! { #[embassy_executor::task] }
}

// ── emit_interval ────────────────────────────────────────────────────────────

/// `Ticker::every(Duration::from_millis(#ms))` interval expression
/// (`source_task.rs` and `sink_task.rs` inline block).
pub fn emit_interval(ms: u64) -> TokenStream {
    let ms_lit = proc_macro2::Literal::u64_suffixed(ms);
    quote! { Ticker::every(Duration::from_millis(#ms_lit)) }
}

// ── emit_now_millis ──────────────────────────────────────────────────────────

/// `embassy_time::Instant::now().as_millis()` expression
/// (`source_task.rs` and `sink_task.rs` inline block).
pub fn emit_now_millis() -> TokenStream {
    quote! { embassy_time::Instant::now().as_millis() }
}

// ── emit_spawn ───────────────────────────────────────────────────────────────

/// `spawner.spawn(#task.expect(#label))` call and the generated
/// `spawn_sources(spawner: &Spawner, …)` signature fragment
/// (`spawn.rs` inline block).
///
/// The `task` argument is the task-fn-call expression (without `.expect()`);
/// `label` is the panic message for pool-exhaustion failures. Embassy wraps
/// it in `spawner.spawn(#task.expect(#label))` to unwrap the `SpawnToken`
/// returned by `#[embassy_executor::task]` functions.
pub fn emit_spawn(task: &TokenStream, label: &str) -> TokenStream {
    quote! { spawner.spawn(#task.expect(#label)); }
}

// ── emit_tap_handoff ─────────────────────────────────────────────────────────

/// `wasm_runtime::init_tap_registry(registry)` call
/// (`taps.rs` inline block — emitted in `setup_tap_registry`).
pub fn emit_tap_handoff() -> TokenStream {
    quote! { wasm_runtime::init_tap_registry(registry); }
}

// ── emit_bus_static ──────────────────────────────────────────────────────────

/// `static #bus: StaticCell<Mutex<NoopRawMutex, #inner>> = StaticCell::new();`
/// (`buses.rs` inline block).
///
/// The `bus` argument is the static ident `TokenStream` (e.g. `BUS_I2C0`);
/// `inner` is the concrete inner bus type.
pub fn emit_bus_static(bus: &TokenStream, inner: &TokenStream) -> TokenStream {
    quote! {
        static #bus: StaticCell<Mutex<NoopRawMutex, #inner>> = StaticCell::new();
    }
}

// ── emit_bus_device_new ──────────────────────────────────────────────────────

/// `I2cDevice::new(#bus)` expression (`spawn.rs` inline block).
///
/// The `bus` argument is the mutex variable `TokenStream`.
pub fn emit_bus_device_new(bus: &TokenStream) -> TokenStream {
    quote! { I2cDevice::new(#bus) }
}

// ── emit_spi_bus_device_new ──────────────────────────────────────────────────

/// `SpiDevice::new(#bus_var, peripherals.#cs_field)` expression
/// (`spawn.rs` inline block — SPI device construction).
///
/// The `bus` argument is the mutex variable `TokenStream`;
/// `cs` is the chip-select field ident on `peripherals`.
pub fn emit_spi_bus_device_new(bus: &TokenStream, cs: &TokenStream) -> TokenStream {
    quote! { SpiDevice::new(#bus, peripherals.#cs) }
}

// ── emit_bus_device_type ─────────────────────────────────────────────────────

/// `I2cDevice<'static, NoopRawMutex, #inner>` / `SpiDevice<…>` wrapper in the
/// task signature (`source_task.rs` inline block).
///
/// The `inner` argument is the concrete bus type `TokenStream`.
pub fn emit_bus_device_type(inner: &TokenStream) -> TokenStream {
    quote! { I2cDevice<'static, NoopRawMutex, #inner> }
}

// ── emit_spi_bus_device_type ─────────────────────────────────────────────────

/// `SpiDevice<'static, NoopRawMutex, #inner, #cs>` SPI wrapper in the task
/// signature (`source_task.rs` inline block).
///
/// The `inner` argument is the concrete SPI bus type; `cs` is the chip-select
/// output pin type.
pub fn emit_spi_bus_device_type(inner: &TokenStream, cs: &TokenStream) -> TokenStream {
    quote! { SpiDevice<'static, NoopRawMutex, #inner, #cs> }
}

// ── emit_bus_init ────────────────────────────────────────────────────────────

/// `let #bus_var = #static_ident.init(Mutex::<NoopRawMutex, #inner>::new(peripherals.#bus_field));`
/// (`spawn.rs` inline block — inside `spawn_sources` body).
///
/// Initialises the shared-bus `StaticCell` into a `Mutex` and stores a
/// reference in `bus_var`.
pub fn emit_bus_init(
    bus_var: &TokenStream,
    static_ident: &TokenStream,
    bus_field: &TokenStream,
    inner: &TokenStream,
) -> TokenStream {
    quote! {
        let #bus_var = #static_ident.init(Mutex::<NoopRawMutex, #inner>::new(peripherals.#bus_field));
    }
}

// ── emit_outlet_handoff ──────────────────────────────────────────────────────

/// `wasm_runtime::init_outlet_registry(registry)` call
/// (`outlets.rs` inline block — emitted in `setup_outlet_registry`).
pub fn emit_outlet_handoff() -> TokenStream {
    quote! { wasm_runtime::init_outlet_registry(registry); }
}
