//! `ChipBackend` trait — the codegen seam between chip-agnostic emission and
//! chip-specific scaffolding.

use indexmap::IndexMap;
use proc_macro2::TokenStream;

use crate::descriptor::DriverSchema;
use crate::manifest::BoardManifest;
use crate::scaffold;
use crate::validate_types::ValidationError;

/// Implemented by each chip-specific codegen binary (e.g. `esp-codegen`,
/// `linux-codegen`). Responsible for emitting concrete peripheral construction
/// code from the board manifest's pin/bus assignments, **and** for supplying the
/// runtime-family token scaffolding used in the generated source tasks.
///
/// Methods are split into three groups:
///
/// - **Required (no default impl):** every backend must provide these. Methods
///   for features that a particular chip cannot support (e.g. `spi_bus_type`
///   on a chip without SPI) should be implemented as a panicking stub —
///   `panic!("this chip has no SPI")` — so the unimplemented case is visible
///   in the backend itself rather than hidden behind a trait default. The
///   codegen only calls SPI / GPIO methods when the manifest references
///   those features, so a panicking stub is reachable only via misconfiguration.
/// - **Optional hooks with sensible defaults:** legitimate no-op behaviour for
///   backends that don't need to customise that aspect.
/// - **Runtime hooks (default → `scaffold::embassy`):** thirteen generative hooks
///   that produce `TokenStream` fragments wired into the generated source/sink
///   tasks. The default implementation delegates to `scaffold::embassy`, so
///   Embassy-based backends (`Esp32Backend`, future `NrfBackend`) write zero
///   runtime code. Linux-targeting backends override each hook with a one-line
///   delegation to `scaffold::tokio`.
pub trait ChipBackend {
    // ── Required: every backend must implement these. ──────────────────────

    /// Chip-specific `use` statements injected at the top of the generated file.
    fn emit_imports(&self) -> TokenStream;

    /// Emit the `BoardPeripherals` struct and its `new(Peripherals)` constructor.
    ///
    /// `driver_schemas` (keyed by driver id) lets the backend distinguish
    /// output-capable devices (those with a `writes` block) so it can emit their
    /// driven pin as a proper output — a digital `Output` or a configured PWM
    /// channel — rather than a bidirectional `Flex`.
    fn emit_board_peripherals(
        &self,
        manifest: &BoardManifest,
        driver_schemas: &IndexMap<String, DriverSchema>,
    ) -> TokenStream;

    /// Concrete inner I2C bus type tokens, used as the type parameter of the
    /// shared-bus `I2cDevice` wrapper.
    /// Example for ESP32: `esp_hal::i2c::master::I2c<'static, esp_hal::Async>`
    fn i2c_bus_type(&self) -> TokenStream;

    /// Concrete inner SPI bus type tokens, used as the type parameter of the
    /// shared-bus `SpiDevice` wrapper. Backends targeting chips without SPI
    /// should implement this with `panic!("this chip has no SPI")` — the
    /// codegen only calls it when the manifest declares an SPI bus.
    /// Example for ESP32: `esp_hal::spi::master::Spi<'static, esp_hal::Async>`
    fn spi_bus_type(&self) -> TokenStream;

    /// Concrete CS output pin type tokens, used as the fourth type parameter of
    /// `SpiDevice`. Same opt-out convention as [`spi_bus_type`](Self::spi_bus_type).
    /// Example for ESP32: `esp_hal::gpio::Output<'static>`
    fn spi_cs_type(&self) -> TokenStream;

    /// Concrete GPIO flexible-pin type tokens, used as the type of named device
    /// pins passed to driver init functions. Backends without device-pin support
    /// should panic — only called when the manifest wires device pins.
    /// Example for ESP32: `esp_hal::gpio::Flex<'static>`
    fn gpio_flex_type(&self) -> TokenStream;

    /// Emit a `pipeline_pins!($p:ident)` macro that constructs the WASM
    /// runtime's `Pins` set with the manifest's reserved bus/device pins
    /// excluded. Return an empty `TokenStream` if the platform has no
    /// pin-to-WASM forwarding (e.g. `LinuxChipBackend`).
    fn emit_pipeline_pins_macro(&self, manifest: &BoardManifest) -> TokenStream;

    // ── Optional hooks with sensible defaults. ────────────────────────────

    /// Concrete digital output pin type tokens, used as the pin type of a GPIO
    /// on/off sink task. Backends without output support should panic — only
    /// called when a pipeline declares a digital output outlet.
    /// Example for ESP32: `esp_hal::gpio::Output<'static>`
    fn gpio_output_type(&self) -> TokenStream {
        panic!("this backend does not support digital GPIO output")
    }

    /// Concrete PWM channel type tokens, used as the pin type of a PWM sink task.
    /// Must implement `embedded_hal::pwm::SetDutyCycle`. Backends without PWM
    /// should panic — only called when a pipeline declares a PWM output outlet.
    /// Example for ESP32: `esp_hal::ledc::channel::Channel<'static, esp_hal::ledc::LowSpeed>`
    fn pwm_channel_type(&self) -> TokenStream {
        panic!("this backend does not support PWM output")
    }

    /// Concrete digital input pin type tokens, used for a hybrid output device's
    /// feedback pin. Must implement `embedded_hal::digital::InputPin`. Backends
    /// without input support should panic — only called when a hybrid output
    /// device declares a feedback pin.
    /// Example for ESP32: `esp_hal::gpio::Input<'static>`
    fn gpio_input_type(&self) -> TokenStream {
        panic!("this backend does not support GPIO input")
    }

    /// Pointer width of the target in bits (32 or 64). Used by the validator
    /// to range-check `usize` config fields against the correct maximum.
    /// Override this when targeting a 64-bit platform (e.g. Linux signal-layer).
    fn pointer_width(&self) -> u32 {
        32
    }

    /// Chip-specific manifest validation: check that bus ids, pin numbers, and
    /// other manifest fields match what the chip actually exposes.
    /// Called during the validation phase alongside `validate_manifest` and
    /// `validate_pipeline_against_manifest`. Default impl accepts everything.
    fn validate_manifest(&self, _manifest: &BoardManifest) -> Vec<ValidationError> {
        vec![]
    }

    // ── Runtime hooks (default → scaffold::embassy). ──────────────────────
    //
    // Thirteen generative hooks that produce `TokenStream` fragments wired into
    // the generated source/sink tasks. The default implementation delegates
    // to `scaffold::embassy` so Embassy-based backends write zero runtime
    // code. Linux backends override each with a `scaffold::tokio` delegation.

    /// Common runtime `use` items injected at the top of the generated file
    /// alongside the chip-specific imports from `emit_imports`.
    ///
    /// **Embassy default:** Embassy shared-bus, executor, sync, time, and
    /// `static_cell` imports.
    fn emit_runtime_imports(&self) -> TokenStream {
        scaffold::embassy::emit_runtime_imports()
    }

    /// The async-task attribute item emitted immediately before each task
    /// function definition. May be empty (`TokenStream::new()`) on platforms
    /// where task functions are plain async fns.
    ///
    /// **Embassy default:** `#[embassy_executor::task]`
    fn emit_task_attribute(&self) -> TokenStream {
        scaffold::embassy::emit_task_attribute()
    }

    /// A `Ticker` / interval constructor expression that fires every `ms`
    /// milliseconds. The returned tokens are an *expression* fragment (no
    /// trailing semicolon).
    ///
    /// **Embassy default:** `Ticker::every(Duration::from_millis(#ms))`
    fn emit_interval(&self, ms: u64) -> TokenStream {
        scaffold::embassy::emit_interval(ms)
    }

    /// An expression that evaluates to the current time as a `u64` of
    /// milliseconds. The returned tokens are an *expression* fragment.
    ///
    /// **Embassy default:** `embassy_time::Instant::now().as_millis()`
    fn emit_now_millis(&self) -> TokenStream {
        scaffold::embassy::emit_now_millis()
    }

    /// A statement that spawns `task`. The `task` argument is the task-fn-call
    /// expression WITHOUT a trailing `.expect()` (e.g. `my_task(arg)`); `label`
    /// is the panic message used by Embassy to unwrap the `SpawnToken`. Tokio
    /// backends ignore `label` because `async fn` returns a `Future` (no
    /// `.expect()` is needed or valid). Covers both individual source/sink spawns
    /// **and** the generated `spawn_sources(spawner: &Spawner, …)` signature —
    /// the backend's choice of spawner type (Embassy `Spawner` vs Tokio) drives
    /// the function signature at the call site.
    ///
    /// **Embassy default:** `spawner.spawn(#task.expect(#label));`
    fn emit_spawn(&self, task: &TokenStream, label: &str) -> TokenStream {
        scaffold::embassy::emit_spawn(task, label)
    }

    /// Statements that hand the completed tap registry to the runtime.
    /// Emitted at the end of `setup_tap_registry`.
    ///
    /// **Embassy default:** `wasm_runtime::init_tap_registry(registry);`
    fn emit_tap_handoff(&self) -> TokenStream {
        scaffold::embassy::emit_tap_handoff()
    }

    /// A static declaration for a shared bus. The `bus` argument is the static
    /// ident `TokenStream` (e.g. `BUS_I2C0`); `inner` is the concrete inner bus
    /// type (e.g. `esp_hal::i2c::master::I2c<'static, esp_hal::Async>`).
    ///
    /// **Embassy default:** `static #bus: StaticCell<Mutex<NoopRawMutex, #inner>> = StaticCell::new();`
    fn emit_bus_static(&self, bus: &TokenStream, inner: &TokenStream) -> TokenStream {
        scaffold::embassy::emit_bus_static(bus, inner)
    }

    /// An expression that constructs a bus device handle from `bus`. The `bus`
    /// argument is the initialised mutex variable `TokenStream`.
    ///
    /// **Embassy default:** `I2cDevice::new(#bus)`
    fn emit_bus_device_new(&self, bus: &TokenStream) -> TokenStream {
        scaffold::embassy::emit_bus_device_new(bus)
    }

    /// An expression that constructs an SPI bus device handle from `bus` and
    /// `cs` (chip-select field on `peripherals`). Backends that do not support
    /// SPI should implement this as `panic!("SPI unreachable on this platform")`.
    ///
    /// **Embassy default:** `SpiDevice::new(#bus_var, peripherals.#cs_field)`
    fn emit_spi_bus_device_new(&self, bus: &TokenStream, cs: &TokenStream) -> TokenStream {
        scaffold::embassy::emit_spi_bus_device_new(bus, cs)
    }

    /// The bus-device wrapper *type* used in the task function signature. The
    /// `inner` argument is the concrete inner bus type (e.g.
    /// `esp_hal::i2c::master::I2c<'static, esp_hal::Async>`).
    ///
    /// **Embassy default:** `I2cDevice<'static, NoopRawMutex, #inner>`
    fn emit_bus_device_type(&self, inner: &TokenStream) -> TokenStream {
        scaffold::embassy::emit_bus_device_type(inner)
    }

    /// The SPI bus-device wrapper *type* used in the task function signature.
    /// The `inner` argument is the concrete inner SPI bus type and `cs` is the
    /// chip-select output pin type.
    ///
    /// **Embassy default:** `SpiDevice<'static, NoopRawMutex, #inner, #cs>`
    fn emit_spi_bus_device_type(&self, inner: &TokenStream, cs: &TokenStream) -> TokenStream {
        scaffold::embassy::emit_spi_bus_device_type(inner, cs)
    }

    /// A statement that initialises a bus mutex from the corresponding
    /// `StaticCell` and stores it in `bus_var`. Arguments:
    /// - `bus_var`: the local variable ident that will hold the mutex reference
    /// - `static_ident`: the static `StaticCell` ident (e.g. `BUS_I2C0`)
    /// - `bus_field`: the `peripherals` field name for this bus
    /// - `inner`: the concrete inner bus type
    ///
    /// **Embassy default:**
    /// `let #bus_var = #static_ident.init(Mutex::<NoopRawMutex, #inner>::new(peripherals.#bus_field));`
    fn emit_bus_init(
        &self,
        bus_var: &TokenStream,
        static_ident: &TokenStream,
        bus_field: &TokenStream,
        inner: &TokenStream,
    ) -> TokenStream {
        scaffold::embassy::emit_bus_init(bus_var, static_ident, bus_field, inner)
    }

    /// Statements that hand the completed outlet registry to the runtime.
    /// Emitted at the end of `setup_outlet_registry`.
    ///
    /// **Embassy default:** `wasm_runtime::init_outlet_registry(registry);`
    fn emit_outlet_handoff(&self) -> TokenStream {
        scaffold::embassy::emit_outlet_handoff()
    }
}
