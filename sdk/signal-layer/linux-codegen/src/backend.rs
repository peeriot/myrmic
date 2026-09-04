//! `LinuxChipBackend` — implements `ChipBackend` for Linux tokio pipelines.
//!
//! All thirteen runtime hooks delegate to `scaffold::tokio`; the three actuator
//! type methods map to `linux-gpio-shim` (character-device GPIO lines and
//! sysfs PWM channels); SPI buses map to spidev nodes with software CS.

use indexmap::IndexMap;
use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;

use pipeline_backend_api::ChipBackend;
use pipeline_backend_api::descriptor::{DriverSchema, OutputMode};
use pipeline_backend_api::manifest::{BoardManifest, BusTransport};
use pipeline_backend_api::scaffold;
use pipeline_backend_api::validate_types::ValidationError;

use crate::linux_manifest::{LinuxManifestOverlay, parse_linux_overlay};

/// GPIO character device used when a device's overlay omits `gpio_chip`.
const DEFAULT_GPIO_CHIP: &str = "/dev/gpiochip0";
/// sysfs PWM chip used when a device's overlay omits `pwm_chip`.
const DEFAULT_PWM_CHIP: &str = "pwmchip0";

/// Backend that generates tokio-based Linux pipeline crates.
///
/// The optional `overlay` carries Linux-specific bus fields (e.g. `dev_path`)
/// that the common `BoardManifest` does not store.
pub struct LinuxChipBackend {
    /// Linux-specific bus overlay parsed from the raw manifest YAML.
    /// `None` in test/validation contexts where the YAML is not available.
    pub overlay: Option<LinuxManifestOverlay>,
}

impl ChipBackend for LinuxChipBackend {
    // ── Required base methods ─────────────────────────────────────────────────

    fn emit_imports(&self) -> TokenStream {
        quote! {
            use linux_i2c_shim::{LinuxI2cdev, SharedI2c};
        }
    }

    #[allow(clippy::too_many_lines)] // codegen: per-device emission loops, not meaningfully splittable
    fn emit_board_peripherals(
        &self,
        manifest: &BoardManifest,
        driver_schemas: &IndexMap<String, DriverSchema>,
    ) -> TokenStream {
        let mut field_decls: Vec<TokenStream> = vec![];
        let mut inits: Vec<TokenStream> = vec![];
        let mut field_inits: Vec<TokenStream> = vec![];

        for (bus_id, bus) in &manifest.buses {
            if bus.transport == BusTransport::Spi {
                // spidev node with the kernel chip-select disabled; CS lines
                // are per-device GPIO fields emitted in the device loop below.
                let field_ident = snake_ident(bus_id);
                let dev_path = self
                    .overlay
                    .as_ref()
                    .and_then(|ov| ov.buses.get(bus_id))
                    .and_then(|b| b.dev_path.clone())
                    .unwrap_or_default();
                let freq_hz = bus.freq_khz.saturating_mul(1000);
                let mode = bus.mode;
                let expect_msg = format!("open {dev_path} (bus {bus_id})");
                field_decls.push(quote! {
                    pub #field_ident: linux_spi_shim::SharedSpiBus<linux_spi_shim::LinuxSpidev>,
                });
                inits.push(quote! {
                    let #field_ident = {
                        // Bus open failure is fatal-by-design in v1, like I2C.
                        let raw = linux_spi_shim::LinuxSpidev::open(#dev_path, #freq_hz, #mode)
                            .expect(#expect_msg);
                        linux_spi_shim::SharedSpiBus::new(raw)
                    };
                });
                field_inits.push(quote! { #field_ident, });
                continue;
            }
            let field_ident = snake_ident(bus_id);
            field_decls.push(quote! {
                pub #field_ident: linux_i2c_shim::SharedI2c<linux_i2c_shim::LinuxI2cdev>,
            });

            // Resolve the dev_path: prefer overlay (exact path from manifest YAML),
            // fall back to deriving from bus id by convention (i2c0 → /dev/i2c-0).
            let dev_path = self
                .overlay
                .as_ref()
                .and_then(|ov| ov.buses.get(bus_id))
                .and_then(|b| b.dev_path.clone())
                .unwrap_or_else(|| manifest_bus_dev_path(bus_id));

            inits.push(quote! {
                let #field_ident = {
                    // S3 / design decision: bus open failure is fatal-by-design in v1.
                    // If the I2C device file is missing or inaccessible the whole
                    // pipeline process terminates here with a clear error message.
                    // Per the resilient-per-device health model this will be replaced
                    // with per-bus Result handling in a future version.
                    let raw = linux_i2c_shim::LinuxI2cdev::open(#dev_path)
                        .expect(concat!("open ", #dev_path));
                    linux_i2c_shim::SharedI2c::new(raw)
                };
            });
            field_inits.push(quote! { #field_ident, });
        }

        // Chip-select lines for SPI devices: one `<id>_cs` GPIO output per
        // device, booted deasserted (high) so no chip is selected at start.
        // The chip comes from the device's Linux overlay (`gpio_chip`).
        for device in &manifest.devices {
            let on_spi = manifest
                .buses
                .get(&device.bus)
                .is_some_and(|b| b.transport == BusTransport::Spi);
            if !on_spi {
                continue;
            }
            let Some(&cs_pin) = device.pins.get("cs") else {
                continue; // validation requires the `cs` pin
            };
            let gpio_chip = self
                .overlay
                .as_ref()
                .and_then(|ov| ov.device(&device.id))
                .and_then(|d| d.gpio_chip.clone())
                .unwrap_or_else(|| DEFAULT_GPIO_CHIP.to_string());
            let cs_field = snake_ident(&format!("{}_cs", device.id));
            let cs_line = u32::from(cs_pin);
            let expect_msg = format!("open {gpio_chip} line {cs_line} ({}.cs)", device.id);
            field_decls.push(quote! {
                pub #cs_field: linux_gpio_shim::LinuxOutputPin,
            });
            inits.push(quote! {
                let #cs_field = linux_gpio_shim::LinuxOutputPin::open(#gpio_chip, #cs_line, true)
                    .expect(#expect_msg);
            });
            field_inits.push(quote! { #cs_field, });
        }

        // Output devices (outlets): the driven pin as a GPIO line or a sysfs
        // PWM channel, plus any feedback pins as inputs. Same field naming as
        // the ESP backend (`<id>_out`, `<id>_<pin>`); the chips the pins live
        // on come from the manifest's Linux overlay, with `/dev/gpiochip0` /
        // `pwmchip0` as defaults. Open failure is fatal-by-design in v1, like
        // the bus loop above.
        for device in &manifest.devices {
            let Some(write) = driver_schemas
                .get(&device.driver)
                .and_then(|s| s.writes.as_ref())
            else {
                continue;
            };
            let Some(&out_pin) = device.pins.get("out") else {
                continue; // validation requires the `out` pin
            };
            let overlay_device = self.overlay.as_ref().and_then(|ov| ov.device(&device.id));
            let gpio_chip = overlay_device
                .and_then(|d| d.gpio_chip.clone())
                .unwrap_or_else(|| DEFAULT_GPIO_CHIP.to_string());
            let field = snake_ident(&format!("{}_out", device.id));

            match write.mode {
                OutputMode::Digital => {
                    // Boot the line at its deasserted (safe/off) level so an
                    // active-low device isn't briefly asserted before init() runs.
                    let deasserted_high = device
                        .hardware
                        .get("active_low")
                        .and_then(serde_yaml::Value::as_bool)
                        .unwrap_or(false);
                    let line = u32::from(out_pin);
                    let expect_msg = format!("open {gpio_chip} line {line} ({}.out)", device.id);
                    field_decls.push(quote! {
                        pub #field: linux_gpio_shim::LinuxOutputPin,
                    });
                    inits.push(quote! {
                        let #field = linux_gpio_shim::LinuxOutputPin::open(
                            #gpio_chip,
                            #line,
                            #deasserted_high,
                        )
                        .expect(#expect_msg);
                    });
                }
                OutputMode::Pwm => {
                    let pwm_chip = overlay_device
                        .and_then(|d| d.pwm_chip.clone())
                        .unwrap_or_else(|| DEFAULT_PWM_CHIP.to_string());
                    let channel = u32::from(out_pin);
                    // freq_khz is declared as a small integer in descriptors
                    // (≤ 40 000 kHz in practice); saturate silently rather than
                    // panic, matching the ESP backend.
                    #[allow(clippy::cast_possible_truncation)]
                    let freq_khz = device
                        .hardware
                        .get("freq_khz")
                        .and_then(serde_yaml::Value::as_u64)
                        .unwrap_or(1) as u32;
                    let expect_msg =
                        format!("open {pwm_chip} channel {channel} ({}.out)", device.id);
                    field_decls.push(quote! {
                        pub #field: linux_gpio_shim::SysfsPwm,
                    });
                    inits.push(quote! {
                        let #field = linux_gpio_shim::SysfsPwm::open(
                            #pwm_chip,
                            #channel,
                            #freq_khz,
                        )
                        .expect(#expect_msg);
                    });
                }
            }
            field_inits.push(quote! { #field, });

            // Feedback input pins (hybrid devices): every declared pin other
            // than `out` is read back as a digital input.
            for (pin_name, &pin_num) in &device.pins {
                if pin_name == "out" {
                    continue;
                }
                let fb_field = snake_ident(&format!("{}_{}", device.id, pin_name));
                let fb_line = u32::from(pin_num);
                let expect_msg =
                    format!("open {gpio_chip} line {fb_line} ({}.{pin_name})", device.id);
                field_decls.push(quote! {
                    pub #fb_field: linux_gpio_shim::LinuxInputPin,
                });
                inits.push(quote! {
                    let #fb_field = linux_gpio_shim::LinuxInputPin::open(#gpio_chip, #fb_line)
                        .expect(#expect_msg);
                });
                field_inits.push(quote! { #fb_field, });
            }
        }

        quote! {
            pub struct BoardPeripherals {
                #(#field_decls)*
            }

            impl BoardPeripherals {
                pub fn new() -> Self {
                    #(#inits)*
                    BoardPeripherals {
                        #(#field_inits)*
                    }
                }
            }

            impl Default for BoardPeripherals {
                fn default() -> Self {
                    Self::new()
                }
            }
        }
    }

    fn i2c_bus_type(&self) -> TokenStream {
        quote! { linux_i2c_shim::SharedI2c<linux_i2c_shim::LinuxI2cdev> }
    }

    fn spi_bus_type(&self) -> TokenStream {
        quote! { linux_spi_shim::LinuxSpidev }
    }

    fn spi_cs_type(&self) -> TokenStream {
        quote! { linux_gpio_shim::LinuxOutputPin }
    }

    fn gpio_flex_type(&self) -> TokenStream {
        panic!("Linux signal-layer does not support flexible GPIO pins in v1")
    }

    fn gpio_output_type(&self) -> TokenStream {
        quote! { linux_gpio_shim::LinuxOutputPin }
    }

    fn pwm_channel_type(&self) -> TokenStream {
        quote! { linux_gpio_shim::SysfsPwm }
    }

    fn gpio_input_type(&self) -> TokenStream {
        quote! { linux_gpio_shim::LinuxInputPin }
    }

    fn emit_pipeline_pins_macro(&self, _manifest: &BoardManifest) -> TokenStream {
        // Linux exposes no pins to cells; the trait doc says "return empty".
        TokenStream::new()
    }

    // ── Optional hooks: pointer width and validation ──────────────────────────

    fn pointer_width(&self) -> u32 {
        64
    }

    fn validate_manifest(&self, manifest: &BoardManifest) -> Vec<ValidationError> {
        // When the overlay is present, use it for path validation.
        // When absent (test/structural-only context), just check SPI rejection.
        validate_linux_manifest_with_overlay(manifest, self.overlay.as_ref())
    }

    // ── Runtime hooks: delegate to scaffold::tokio ────────────────────────────

    fn emit_runtime_imports(&self) -> TokenStream {
        let base = scaffold::tokio::emit_runtime_imports();
        // Define `Spawner` as a type alias for `()` so the generated
        // `spawn_sources(spawner: &Spawner, ...)` compiles on Linux.
        // `spawner` is never used in the body (emit_spawn → tokio::spawn).
        quote! {
            #base
            /// Linux placeholder: Embassy `Spawner` is not used on Linux.
            /// Present so the generated `spawn_sources` signature compiles.
            type Spawner = ();
        }
    }

    fn emit_task_attribute(&self) -> TokenStream {
        scaffold::tokio::emit_task_attribute()
    }

    fn emit_interval(&self, ms: u64) -> TokenStream {
        scaffold::tokio::emit_interval(ms)
    }

    fn emit_now_millis(&self) -> TokenStream {
        scaffold::tokio::emit_now_millis()
    }

    fn emit_spawn(&self, task: &TokenStream, label: &str) -> TokenStream {
        scaffold::tokio::emit_spawn(task, label)
    }

    fn emit_tap_handoff(&self) -> TokenStream {
        emit_linux_tap_handoff()
    }

    fn emit_bus_static(&self, bus: &TokenStream, inner: &TokenStream) -> TokenStream {
        scaffold::tokio::emit_bus_static(bus, inner)
    }

    fn emit_bus_device_new(&self, bus: &TokenStream) -> TokenStream {
        scaffold::tokio::emit_bus_device_new(bus)
    }

    fn emit_spi_bus_device_new(&self, bus: &TokenStream, cs: &TokenStream) -> TokenStream {
        scaffold::tokio::emit_spi_bus_device_new(bus, cs)
    }

    fn emit_bus_device_type(&self, inner: &TokenStream) -> TokenStream {
        scaffold::tokio::emit_bus_device_type(inner)
    }

    fn emit_spi_bus_device_type(&self, inner: &TokenStream, cs: &TokenStream) -> TokenStream {
        scaffold::tokio::emit_spi_bus_device_type(inner, cs)
    }

    fn emit_bus_init(
        &self,
        bus_var: &TokenStream,
        _static_ident: &TokenStream,
        bus_field: &TokenStream,
        _inner: &TokenStream,
    ) -> TokenStream {
        // Linux: no StaticCell — take the bus directly from BoardPeripherals.
        // `bus_field` is the `peripherals.i2c0` field name (as a TokenStream),
        // `bus_var` is the local variable name (e.g. `i2c0_mutex`).
        quote! {
            let #bus_var = peripherals.#bus_field;
        }
    }

    fn emit_outlet_handoff(&self) -> TokenStream {
        // Linux: wrap the registry in an `OutletStore` adapter and park it in
        // signal-layer-linux-rt for the IPC server, which `setup_tap_registry`
        // starts afterwards (the generated main calls the two setup functions
        // in that order). Feed-forward outlets are applied inline in their
        // source task and never reach the registry, so for pipelines without
        // cell-driven outlets this parks an empty store (resolve → NotFound).
        //
        // Writes are stamped with the same fenced time seam the sink tasks
        // read (`now_millis`), so slot-timestamp change detection and driver
        // rate limiting compare values from one clock.
        quote! {
            // ── Inline OutletStore adapter ─────────────────────────────────
            // `OutletRegistry` (signal-layer-core, no_std) and `OutletStore`
            // (signal-layer-ipc, std) are in separate crates with no mutual
            // dependency. Generate a local newtype that satisfies the trait.
            struct OutletRegistryStore(signal_layer_core::OutletRegistry);

            impl signal_layer_ipc::OutletStore for OutletRegistryStore {
                fn resolve(&self, name: &str) -> Option<u32> {
                    self.0.resolve(name)
                }

                fn write(&self, h: u32, bytes: &[u8]) -> signal_layer_ipc::StoreWrite {
                    let Some(outlet) = self.0.get(h) else {
                        return signal_layer_ipc::StoreWrite::InvalidHandle;
                    };
                    let ts = signal_layer_core::Timestamp(
                        signal_layer_linux_rt::time::now_millis(),
                    );
                    match outlet.write_bytes(ts, bytes) {
                        Ok(()) => signal_layer_ipc::StoreWrite::Ok,
                        Err(signal_layer_core::TapError::Decode) => {
                            signal_layer_ipc::StoreWrite::Rejected
                        }
                        Err(_) => signal_layer_ipc::StoreWrite::InvalidHandle,
                    }
                }

                fn list_len(&self) -> u32 {
                    self.0.len() as u32
                }

                fn list_entry(&self, index: u32) -> Option<(String, u8)> {
                    let name = self.0.name_at(index)?;
                    let kind = self.0.get(index)?.kind() as u8;
                    Some((name.to_string(), kind))
                }

                fn type_id(&self, h: u32) -> Option<u32> {
                    self.0.get(h).map(signal_layer_core::OutletEntry::wire_type_id)
                }
            }

            signal_layer_linux_rt::set_outlet_store(
                std::sync::Arc::new(OutletRegistryStore(registry)),
            );
        }
    }
}

/// Emit the Linux tap-handoff tokens: hand the tap registry to the IPC server.
///
/// The generated binary calls `signal_layer_linux_rt::run_tap_server(socket_path, store)`
/// and then spawns it as a background task to serve taps over a Unix-domain socket.
///
/// Because `TapRegistry` (from `signal-layer-core`) does not implement `TapStore`
/// (from `signal-layer-ipc`) — they are separate crates — we generate a local
/// newtype adapter `TapRegistryStore` that bridges the two and satisfies the
/// `Arc<dyn TapStore>` argument of `run_tap_server`.
pub fn emit_linux_tap_handoff() -> TokenStream {
    quote! {
        // ── Inline TapStore adapter ────────────────────────────────────────
        // `TapRegistry` (signal-layer-core, no_std) and `TapStore`
        // (signal-layer-ipc, std) are in separate crates with no mutual
        // dependency.  Generate a local newtype that satisfies the trait.
        struct TapRegistryStore(signal_layer_core::TapRegistry);

        impl signal_layer_ipc::TapStore for TapRegistryStore {
            fn resolve(&self, name: &str) -> Option<u32> {
                self.0.resolve(name)
            }

            fn read_retained(&self, h: u32) -> signal_layer_ipc::StoreRead {
                match self.0.get(h) {
                    Some(signal_layer_core::SlotEntry::Retained(r)) => {
                        let mut ts = 0u64;
                        let mut buf = [0u8; 256];
                        match r.read_bytes(&mut ts, &mut buf) {
                            Ok(n) => signal_layer_ipc::StoreRead::Value {
                                timestamp_ms: ts,
                                bytes: buf[..n].to_vec(),
                            },
                            Err(signal_layer_core::TapError::Empty) => {
                                signal_layer_ipc::StoreRead::Empty
                            }
                            Err(_) => signal_layer_ipc::StoreRead::InvalidHandle,
                        }
                    }
                    _ => signal_layer_ipc::StoreRead::InvalidHandle,
                }
            }

            fn take_event(&self, h: u32) -> signal_layer_ipc::StoreRead {
                match self.0.get(h) {
                    Some(signal_layer_core::SlotEntry::Event(e)) => {
                        let mut buf = [0u8; 256];
                        match e.take_bytes(&mut buf) {
                            Ok(n) => signal_layer_ipc::StoreRead::Value {
                                timestamp_ms: 0,
                                bytes: buf[..n].to_vec(),
                            },
                            Err(signal_layer_core::TapError::Empty) => {
                                signal_layer_ipc::StoreRead::Empty
                            }
                            Err(_) => signal_layer_ipc::StoreRead::InvalidHandle,
                        }
                    }
                    _ => signal_layer_ipc::StoreRead::InvalidHandle,
                }
            }

            fn list_len(&self) -> u32 {
                self.0.len() as u32
            }

            fn list_entry(&self, index: u32) -> Option<(String, u8)> {
                let name = self.0.name_at(index)?;
                let kind = self.0.get(index)?.kind() as u8;
                Some((name.to_string(), kind))
            }

            fn type_id(&self, h: u32) -> Option<u32> {
                self.0.get(h).map(signal_layer_core::SlotEntry::wire_type_id)
            }
        }

        let socket_path = signal_layer_ipc::default_socket_path()
            .expect("no socket path available: set XDG_RUNTIME_DIR or ensure /run/peeriot is writable");
        // The outlet store was parked by `setup_outlet_registry()`, which the
        // generated main calls before `setup_tap_registry()`; `None` here means
        // a violated setup order, and outlet requests answer Unsupported.
        let _server = tokio::spawn(
            signal_layer_linux_rt::run_signal_server(
                socket_path,
                std::sync::Arc::new(TapRegistryStore(registry)),
                signal_layer_linux_rt::take_outlet_store(),
            )
        );
    }
}

/// Validate a Linux manifest.
///
/// `yaml` may be `Some(raw_yaml_str)` so the Linux overlay fields (like
/// `dev_path`) can be checked; pass `None` to skip overlay checks (used in
/// unit tests that work with a pre-parsed `BoardManifest`).
pub fn validate_linux_manifest(
    manifest: &BoardManifest,
    yaml: Option<&str>,
) -> Vec<ValidationError> {
    let overlay = yaml.and_then(|y| parse_linux_overlay(y).ok());
    validate_linux_manifest_with_overlay(manifest, overlay.as_ref())
}

/// Validate a Linux manifest using a pre-parsed overlay (may be `None`).
pub fn validate_linux_manifest_with_overlay(
    manifest: &BoardManifest,
    overlay: Option<&LinuxManifestOverlay>,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // SR-4: validate /dev/i2c-* paths when overlay is available.
    if let Some(ov) = overlay {
        for (bus_id, bus) in &manifest.buses {
            if bus.transport != BusTransport::I2c {
                continue;
            }
            let overlay_bus = ov.buses.get(bus_id);
            let dev_path_opt = overlay_bus.and_then(|b| b.dev_path.as_deref());
            match dev_path_opt {
                None => {
                    errors.push(ValidationError::new(format!(
                        "bus `{bus_id}`: missing `dev_path` — Linux I2C buses require \
                         a `dev_path: /dev/i2c-N` field in the manifest"
                    )));
                }
                Some(path) => {
                    if let Err(e) = validate_i2c_dev_path(path) {
                        errors.push(ValidationError::new(format!("bus `{bus_id}`: {e}")));
                    }
                }
            }
        }

        // SPI buses require an explicit spidev node (no naming convention to
        // fall back on, unlike i2c-N).
        for (bus_id, bus) in &manifest.buses {
            if bus.transport != BusTransport::Spi {
                continue;
            }
            match ov.buses.get(bus_id).and_then(|b| b.dev_path.as_deref()) {
                None => {
                    errors.push(ValidationError::new(format!(
                        "bus `{bus_id}`: missing `dev_path` — Linux SPI buses require \
                         a `dev_path: /dev/spidevB.C` field in the manifest"
                    )));
                }
                Some(path) => {
                    if let Err(e) = validate_spi_dev_path(path) {
                        errors.push(ValidationError::new(format!("bus `{bus_id}`: {e}")));
                    }
                }
            }
        }

        // Device overlay checks: `gpio_chip` / `pwm_chip` must be well-formed
        // when present. Devices without an overlay entry fall back to the
        // defaults, so nothing is required here.
        for overlay_device in &ov.devices {
            let id = &overlay_device.id;
            if let Some(chip) = overlay_device.gpio_chip.as_deref()
                && let Err(e) = validate_gpio_chip_path(chip)
            {
                errors.push(ValidationError::new(format!("device `{id}`: {e}")));
            }
            if let Some(chip) = overlay_device.pwm_chip.as_deref()
                && let Err(e) = validate_pwm_chip_name(chip)
            {
                errors.push(ValidationError::new(format!("device `{id}`: {e}")));
            }
        }
    }

    errors
}

/// Validate that `path` is a well-formed `/dev/gpiochipN` path.
pub fn validate_gpio_chip_path(path: &str) -> Result<(), String> {
    let suffix = path.strip_prefix("/dev/gpiochip").ok_or_else(|| {
        format!(
            "invalid GPIO chip path `{path}`: expected `/dev/gpiochipN` format \
             (e.g. `/dev/gpiochip0`)"
        )
    })?;
    if suffix.is_empty() || suffix.parse::<u32>().is_err() {
        return Err(format!(
            "invalid GPIO chip path `{path}`: chip number `{suffix}` is not a \
             non-negative integer"
        ));
    }
    Ok(())
}

/// Validate that `name` is a well-formed sysfs `pwmchipN` name.
pub fn validate_pwm_chip_name(name: &str) -> Result<(), String> {
    let suffix = name.strip_prefix("pwmchip").ok_or_else(|| {
        format!(
            "invalid PWM chip name `{name}`: expected `pwmchipN` format \
             (e.g. `pwmchip0`)"
        )
    })?;
    if suffix.is_empty() || suffix.parse::<u32>().is_err() {
        return Err(format!(
            "invalid PWM chip name `{name}`: chip number `{suffix}` is not a \
             non-negative integer"
        ));
    }
    Ok(())
}

/// Validate that `path` is a well-formed `/dev/i2c-N` path.
///
/// Accepts `/dev/i2c-N` where N is a non-negative integer.
pub fn validate_i2c_dev_path(path: &str) -> Result<(), String> {
    let stripped = path.strip_prefix("/dev/i2c-").ok_or_else(|| {
        format!(
            "invalid I2C device path `{path}`: expected `/dev/i2c-N` format \
             (e.g. `/dev/i2c-1`)"
        )
    })?;

    if stripped.is_empty() {
        return Err(format!(
            "invalid I2C device path `{path}`: missing bus number after `/dev/i2c-`"
        ));
    }

    stripped.parse::<u32>().map(|_| ()).map_err(|_| {
        format!(
            "invalid I2C device path `{path}`: bus number `{stripped}` is not a \
             non-negative integer"
        )
    })
}

/// Validate that `path` is a well-formed `/dev/spidevB.C` path.
pub fn validate_spi_dev_path(path: &str) -> Result<(), String> {
    let suffix = path.strip_prefix("/dev/spidev").ok_or_else(|| {
        format!(
            "invalid SPI device path `{path}`: expected `/dev/spidevB.C` format \
             (e.g. `/dev/spidev0.0`)"
        )
    })?;
    let well_formed = suffix
        .split_once('.')
        .is_some_and(|(b, c)| b.parse::<u32>().is_ok() && c.parse::<u32>().is_ok());
    if !well_formed {
        return Err(format!(
            "invalid SPI device path `{path}`: `{suffix}` is not `<bus>.<cs>` \
             with non-negative integers"
        ));
    }
    Ok(())
}

/// Derive a `/dev/i2c-N` path from a bus id by convention.
///
/// `i2c0` → `/dev/i2c-0`, `i2c1` → `/dev/i2c-1`, etc.
/// This is only called during code emission (after validation has passed),
/// so the bus id is guaranteed to parse correctly.
fn manifest_bus_dev_path(bus_id: &str) -> String {
    // Try to extract the numeric suffix from "i2cN".
    let suffix = bus_id
        .strip_prefix("i2c")
        .unwrap_or("0")
        .parse::<u32>()
        .unwrap_or(0);
    format!("/dev/i2c-{suffix}")
}

/// Convert a `snake_case` or kebab-case string to a `proc_macro2::Ident`.
fn snake_ident(s: &str) -> Ident {
    let snake = s.replace('-', "_");
    Ident::new(&snake, Span::call_site())
}

/// Validate a Linux manifest from raw YAML and return the parsed `BoardManifest`
/// and any validation errors.
///
/// This is a convenience wrapper used by the CLI.
pub fn validate_linux_manifest_from_yaml(
    yaml: &str,
) -> (
    Result<BoardManifest, serde_yaml::Error>,
    Vec<ValidationError>,
) {
    match pipeline_backend_api::manifest::parse_manifest(yaml) {
        Ok(manifest) => {
            let errs = validate_linux_manifest(&manifest, Some(yaml));
            (Ok(manifest), errs)
        }
        Err(e) => (Err(e), vec![]),
    }
}

impl LinuxChipBackend {
    /// Create a backend with no overlay (structural validation only).
    pub fn new() -> Self {
        Self { overlay: None }
    }

    /// Create a backend with an overlay parsed from the manifest YAML.
    pub fn with_overlay(overlay: LinuxManifestOverlay) -> Self {
        Self {
            overlay: Some(overlay),
        }
    }
}

impl Default for LinuxChipBackend {
    fn default() -> Self {
        Self::new()
    }
}
