//! ESP32 chip backend — implements `pipeline_codegen::ChipBackend` for ESP32
//! targets (specifically esp32c6 with esp-hal).

use indexmap::IndexMap;
use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::quote;

use pipeline_codegen::ChipBackend;
use pipeline_codegen::descriptor::{DriverSchema, OutputMode};
use pipeline_codegen::manifest::{BoardManifest, BusTransport};

pub struct Esp32Backend;

impl ChipBackend for Esp32Backend {
    fn emit_imports(&self) -> TokenStream {
        quote! {
            use esp_hal::gpio::{Level, Output};
            use esp_hal::i2c::master::I2c;
            use esp_hal::spi::master::Spi;
            use esp_hal::Async;
        }
    }

    // The function handles every combination of bus transport × device kind ×
    // pin role in a single sequential pass; splitting it further would require
    // passing many state variables between sub-functions, which buys no clarity.
    #[allow(clippy::too_many_lines)]
    fn emit_board_peripherals(
        &self,
        manifest: &BoardManifest,
        driver_schemas: &IndexMap<String, DriverSchema>,
    ) -> TokenStream {
        let mut field_decls: Vec<TokenStream> = vec![];
        let mut new_params: Vec<TokenStream> = vec![];
        let mut constructions: Vec<TokenStream> = vec![];
        let mut field_inits: Vec<TokenStream> = vec![];
        // `$p.FIELD` accessors for the generated macro, in `new` parameter order.
        let mut macro_args: Vec<TokenStream> = vec![];
        // Module-level statics (StaticCells) emitted alongside the struct.
        let mut extra_statics: Vec<TokenStream> = vec![];

        // Which devices are output-capable (their driven pin is an Output/PWM
        // channel, handled in a dedicated loop below — not as a bidirectional Flex).
        let is_output_device = |device: &pipeline_codegen::manifest::DeviceEntry| {
            driver_schemas
                .get(&device.driver)
                .and_then(|s| s.writes.as_ref())
                .is_some()
        };

        for (bus_id, bus) in &manifest.buses {
            let field_ident = snake_ident(bus_id);
            match bus.transport {
                BusTransport::I2c => {
                    field_decls.push(quote! {
                        pub #field_ident: esp_hal::i2c::master::I2c<'static, esp_hal::Async>,
                    });

                    // Each bus peripheral is taken by value so the caller keeps
                    // ownership of the rest of `Peripherals` (WIFI, timers, …).
                    // Bus id → peripheral name: `i2c0` → `I2C0`, `i2c1` → `I2C1`.
                    let periph_name = bus_id.to_uppercase();
                    let periph_ident = Ident::new(&periph_name, Span::call_site());
                    let i2c_ty = quote!(esp_hal::peripherals::#periph_ident<'static>);
                    let i2c_field = periph_ident;
                    let freq_lit = Literal::u32_suffixed(bus.freq_khz);
                    let scl_pin = bus.pins.get("scl").copied().unwrap_or(0);
                    let sda_pin = bus.pins.get("sda").copied().unwrap_or(0);
                    let scl_ty = gpio_ty(scl_pin);
                    let sda_ty = gpio_ty(sda_pin);
                    let scl_field = Ident::new(&format!("GPIO{scl_pin}"), Span::call_site());
                    let sda_field = Ident::new(&format!("GPIO{sda_pin}"), Span::call_site());

                    let i2c_param = field_ident.clone();
                    let scl_param = snake_ident(&format!("{bus_id}_scl"));
                    let sda_param = snake_ident(&format!("{bus_id}_sda"));

                    new_params.push(quote! { #i2c_param: #i2c_ty, });
                    new_params.push(quote! { #scl_param: #scl_ty, });
                    new_params.push(quote! { #sda_param: #sda_ty, });

                    macro_args.push(quote! { $p.#i2c_field, });
                    macro_args.push(quote! { $p.#scl_field, });
                    macro_args.push(quote! { $p.#sda_field, });

                    constructions.push(quote! {
                        let #field_ident = esp_hal::i2c::master::I2c::new(
                            #i2c_param,
                            esp_hal::i2c::master::Config::default()
                                .with_frequency(esp_hal::time::Rate::from_khz(#freq_lit)),
                        )
                        .unwrap()
                        .with_scl(#scl_param)
                        .with_sda(#sda_param)
                        .into_async();
                    });

                    field_inits.push(quote! { #field_ident, });
                }
                BusTransport::Spi => {
                    // The bus ID must match the peripheral name (e.g. "spi2" → SPI2).
                    let spi_periph_name = bus_id.to_uppercase();
                    let spi_periph_ty = Ident::new(&spi_periph_name, Span::call_site());
                    let spi_periph_field = Ident::new(&spi_periph_name, Span::call_site());

                    let sclk_pin = bus.pins.get("sclk").copied().unwrap_or(0);
                    let mosi_pin = bus.pins.get("mosi").copied().unwrap_or(0);
                    let miso_pin = bus.pins.get("miso").copied();

                    let sclk_ty = gpio_ty(sclk_pin);
                    let mosi_ty = gpio_ty(mosi_pin);
                    let sclk_gpio = Ident::new(&format!("GPIO{sclk_pin}"), Span::call_site());
                    let mosi_gpio = Ident::new(&format!("GPIO{mosi_pin}"), Span::call_site());

                    let spi_param = field_ident.clone();
                    let sclk_param = snake_ident(&format!("{bus_id}_sclk"));
                    let mosi_param = snake_ident(&format!("{bus_id}_mosi"));

                    let freq_lit = Literal::u32_suffixed(bus.freq_khz);
                    let mode_variant = spi_mode_variant(bus.mode);

                    field_decls.push(quote! {
                        pub #field_ident: esp_hal::spi::master::Spi<'static, esp_hal::Async>,
                    });

                    new_params.push(
                        quote! { #spi_param: esp_hal::peripherals::#spi_periph_ty<'static>, },
                    );
                    new_params.push(quote! { #sclk_param: #sclk_ty, });
                    new_params.push(quote! { #mosi_param: #mosi_ty, });

                    macro_args.push(quote! { $p.#spi_periph_field, });
                    macro_args.push(quote! { $p.#sclk_gpio, });
                    macro_args.push(quote! { $p.#mosi_gpio, });

                    let with_miso = if let Some(miso_pin) = miso_pin {
                        let miso_ty = gpio_ty(miso_pin);
                        let miso_gpio = Ident::new(&format!("GPIO{miso_pin}"), Span::call_site());
                        let miso_param = snake_ident(&format!("{bus_id}_miso"));
                        new_params.push(quote! { #miso_param: #miso_ty, });
                        macro_args.push(quote! { $p.#miso_gpio, });
                        quote! { .with_miso(#miso_param) }
                    } else {
                        quote! {}
                    };

                    constructions.push(quote! {
                        let #field_ident = esp_hal::spi::master::Spi::new(
                            #spi_param,
                            esp_hal::spi::master::Config::default()
                                .with_frequency(esp_hal::time::Rate::from_khz(#freq_lit))
                                .with_mode(esp_hal::spi::#mode_variant),
                        )
                        .unwrap()
                        .with_sck(#sclk_param)
                        .with_mosi(#mosi_param)
                        #with_miso
                        .into_async();
                    });

                    field_inits.push(quote! { #field_ident, });
                }
            }
        }

        // CS output pins for each device wired to an SPI bus.
        for device in &manifest.devices {
            let Some(bus) = manifest.buses.get(&device.bus) else {
                continue;
            };
            if bus.transport != BusTransport::Spi {
                continue;
            }
            let Some(&cs_pin) = device.pins.get("cs") else {
                continue;
            };
            let cs_field = snake_ident(&format!("{}_cs", device.id));
            let cs_param = snake_ident(&format!("{}_cs_gpio", device.id));
            let cs_gpio_ty = gpio_ty(cs_pin);
            let cs_gpio_field = Ident::new(&format!("GPIO{cs_pin}"), Span::call_site());

            field_decls.push(quote! {
                pub #cs_field: esp_hal::gpio::Output<'static>,
            });
            new_params.push(quote! { #cs_param: #cs_gpio_ty, });
            constructions.push(quote! {
                let #cs_field = esp_hal::gpio::Output::new(
                    #cs_param,
                    esp_hal::gpio::Level::High,
                    esp_hal::gpio::OutputConfig::default(),
                );
            });
            field_inits.push(quote! { #cs_field, });
            macro_args.push(quote! { $p.#cs_gpio_field, });
        }

        // Named GPIO pins for devices (e.g. interrupt pins). CS pins for SPI
        // devices are already emitted above as Output<'static>. Output devices'
        // pins are handled in the dedicated output loop below.
        for device in &manifest.devices {
            if is_output_device(device) {
                continue;
            }
            let is_spi = manifest
                .buses
                .get(&device.bus)
                .is_some_and(|b| b.transport == BusTransport::Spi);
            for (pin_name, &pin_num) in &device.pins {
                if is_spi && pin_name == "cs" {
                    continue;
                }
                let field_ident = snake_ident(&format!("{}_{}", device.id, pin_name));
                let param_ident = snake_ident(&format!("{}_{}_gpio", device.id, pin_name));
                let pin_ty = gpio_ty(pin_num);
                let gpio_field = Ident::new(&format!("GPIO{pin_num}"), Span::call_site());
                field_decls.push(quote! {
                    pub #field_ident: esp_hal::gpio::Flex<'static>,
                });
                new_params.push(quote! { #param_ident: #pin_ty, });
                constructions.push(quote! {
                    let #field_ident = esp_hal::gpio::Flex::new(#param_ident);
                });
                field_inits.push(quote! { #field_ident, });
                macro_args.push(quote! { $p.#gpio_field, });
            }
        }

        // Output devices (Outlets): emit the driven pin as a real output —
        // a digital `Output` or a configured LEDC PWM channel.
        let mut ledc_emitted = false;
        let mut pwm_index: u8 = 0;
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
            let field = snake_ident(&format!("{}_out", device.id));
            let param = snake_ident(&format!("{}_out_gpio", device.id));
            let pin_ty = gpio_ty(out_pin);
            let gpio_field = Ident::new(&format!("GPIO{out_pin}"), Span::call_site());
            new_params.push(quote! { #param: #pin_ty, });
            macro_args.push(quote! { $p.#gpio_field, });

            match write.mode {
                OutputMode::Digital => {
                    // Boot the pin in its deasserted (safe/off) level so an
                    // active-low device isn't briefly asserted before init() runs.
                    let deasserted_level = if device
                        .hardware
                        .get("active_low")
                        .and_then(serde_yaml::Value::as_bool)
                        .unwrap_or(false)
                    {
                        quote! { esp_hal::gpio::Level::High }
                    } else {
                        quote! { esp_hal::gpio::Level::Low }
                    };
                    field_decls.push(quote! {
                        pub #field: esp_hal::gpio::Output<'static>,
                    });
                    constructions.push(quote! {
                        let #field = esp_hal::gpio::Output::new(
                            #param,
                            #deasserted_level,
                            esp_hal::gpio::OutputConfig::default(),
                        );
                    });
                    field_inits.push(quote! { #field, });
                }
                OutputMode::Pwm => {
                    // Shared LEDC instance + peripheral param, emitted once.
                    if !ledc_emitted {
                        ledc_emitted = true;
                        new_params.push(quote! { ledc: esp_hal::peripherals::LEDC<'static>, });
                        macro_args.push(quote! { $p.LEDC, });
                        extra_statics.push(quote! {
                            static PIPELINE_LEDC: StaticCell<esp_hal::ledc::Ledc<'static>> =
                                StaticCell::new();
                        });
                        constructions.push(quote! {
                            // LEDC `configure()` methods are trait methods, so the
                            // traits must be in scope. Emitted here rather than in
                            // the prelude because `esp_hal::ledc` does not exist on
                            // chips without a LEDC driver (C5, C61).
                            use esp_hal::ledc::channel::ChannelIFace;
                            use esp_hal::ledc::timer::TimerIFace;

                            let ledc: &'static mut esp_hal::ledc::Ledc<'static> =
                                PIPELINE_LEDC.init(esp_hal::ledc::Ledc::new(ledc));
                            ledc.set_global_slow_clock(esp_hal::ledc::LSGlobalClkSource::APBClk);
                        });
                    }
                    if pwm_index >= 4 {
                        constructions.push(quote! {
                            compile_error!(
                                "esp32c6 LEDC supports at most 4 PWM output channels"
                            );
                        });
                        continue;
                    }
                    let timer_variant = Ident::new(&format!("Timer{pwm_index}"), Span::call_site());
                    let channel_variant =
                        Ident::new(&format!("Channel{pwm_index}"), Span::call_site());
                    let timer_static = Ident::new(
                        &format!("PWM_TIMER_{}", device.id.to_uppercase().replace('-', "_")),
                        Span::call_site(),
                    );
                    let timer_var = snake_ident(&format!("{}_pwm_timer", device.id));
                    // freq_khz is declared as a small integer in descriptors
                    // (≤ 40 000 kHz in practice); saturate silently rather than
                    // panic, which matches the original behaviour.
                    #[allow(clippy::cast_possible_truncation)]
                    let freq_khz = device
                        .hardware
                        .get("freq_khz")
                        .and_then(serde_yaml::Value::as_u64)
                        .unwrap_or(1) as u32;
                    let freq_lit = Literal::u32_suffixed(freq_khz);
                    extra_statics.push(quote! {
                        static #timer_static: StaticCell<
                            esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::LowSpeed>,
                        > = StaticCell::new();
                    });
                    constructions.push(quote! {
                        let #timer_var = #timer_static.init(
                            ledc.timer::<esp_hal::ledc::LowSpeed>(
                                esp_hal::ledc::timer::Number::#timer_variant,
                            ),
                        );
                        #timer_var
                            .configure(esp_hal::ledc::timer::config::Config {
                                duty: esp_hal::ledc::timer::config::Duty::Duty10Bit,
                                clock_source: esp_hal::ledc::timer::LSClockSource::APBClk,
                                frequency: esp_hal::time::Rate::from_khz(#freq_lit),
                            })
                            .unwrap();
                        // Reborrow the &'static mut as a &'static shared ref so the
                        // channel can hold it for its own 'static lifetime.
                        let #timer_var: &'static esp_hal::ledc::timer::Timer<
                            'static,
                            esp_hal::ledc::LowSpeed,
                        > = #timer_var;
                        let mut #field = ledc.channel::<esp_hal::ledc::LowSpeed>(
                            esp_hal::ledc::channel::Number::#channel_variant,
                            #param,
                        );
                        #field
                            .configure(esp_hal::ledc::channel::config::Config {
                                timer: #timer_var,
                                duty_pct: 0,
                                drive_mode: esp_hal::gpio::DriveMode::PushPull,
                            })
                            .unwrap();
                    });
                    field_decls.push(quote! {
                        pub #field: esp_hal::ledc::channel::Channel<'static, esp_hal::ledc::LowSpeed>,
                    });
                    field_inits.push(quote! { #field, });
                    pwm_index += 1;
                }
            }

            // Feedback input pins (hybrid devices): every declared pin other
            // than `out` is read back as a digital input.
            for (pin_name, &pin_num) in &device.pins {
                if pin_name == "out" {
                    continue;
                }
                let fb_field = snake_ident(&format!("{}_{}", device.id, pin_name));
                let fb_param = snake_ident(&format!("{}_{}_gpio", device.id, pin_name));
                let fb_ty = gpio_ty(pin_num);
                let fb_gpio = Ident::new(&format!("GPIO{pin_num}"), Span::call_site());
                field_decls.push(quote! {
                    pub #fb_field: esp_hal::gpio::Input<'static>,
                });
                new_params.push(quote! { #fb_param: #fb_ty, });
                constructions.push(quote! {
                    let #fb_field = esp_hal::gpio::Input::new(
                        #fb_param,
                        esp_hal::gpio::InputConfig::default(),
                    );
                });
                field_inits.push(quote! { #fb_field, });
                macro_args.push(quote! { $p.#fb_gpio, });
            }
        }

        quote! {
            #(#extra_statics)*

            pub struct BoardPeripherals {
                #(#field_decls)*
            }

            impl BoardPeripherals {
                pub fn new(#(#new_params)*) -> Self {
                    #(#constructions)*
                    BoardPeripherals {
                        #(#field_inits)*
                    }
                }
            }

            /// Build [`BoardPeripherals`] from the chip `Peripherals`, moving only the
            /// bus peripherals this pipeline uses so the caller keeps ownership of the
            /// rest. Pin/peripheral selection lives here (driven by the board manifest),
            /// not in the firmware — callers just write `pipeline_board_peripherals!(p)`.
            #[macro_export]
            macro_rules! pipeline_board_peripherals {
                ($p:ident) => {
                    $crate::pipeline_config::BoardPeripherals::new(#(#macro_args)*)
                };
            }
        }
    }

    fn i2c_bus_type(&self) -> TokenStream {
        quote!(esp_hal::i2c::master::I2c<'static, esp_hal::Async>)
    }

    fn spi_bus_type(&self) -> TokenStream {
        quote!(esp_hal::spi::master::Spi<'static, esp_hal::Async>)
    }

    fn spi_cs_type(&self) -> TokenStream {
        quote!(esp_hal::gpio::Output<'static>)
    }

    fn gpio_flex_type(&self) -> TokenStream {
        quote!(esp_hal::gpio::Flex<'static>)
    }

    fn gpio_output_type(&self) -> TokenStream {
        quote!(esp_hal::gpio::Output<'static>)
    }

    fn pwm_channel_type(&self) -> TokenStream {
        quote!(esp_hal::ledc::channel::Channel<'static, esp_hal::ledc::LowSpeed>)
    }

    fn gpio_input_type(&self) -> TokenStream {
        quote!(esp_hal::gpio::Input<'static>)
    }

    fn emit_pipeline_pins_macro(&self, manifest: &BoardManifest) -> TokenStream {
        let mut reserved: std::collections::BTreeSet<u8> = std::collections::BTreeSet::new();
        for bus in manifest.buses.values() {
            for &pin in bus.pins.values() {
                reserved.insert(pin);
            }
        }
        for device in &manifest.devices {
            for &pin in device.pins.values() {
                reserved.insert(pin);
            }
        }

        let gp_set: std::collections::BTreeSet<u8> =
            manifest.gpios.general_purpose.iter().copied().collect();
        let layout = chip_pin_layout(&manifest.chip)
            .expect("chip is validated before emission; see Esp32Backend::validate_manifest");
        let slots: Vec<TokenStream> = layout
            .iter()
            .map(|slot| match slot {
                Some(pin) if !reserved.contains(pin) && gp_set.contains(pin) => {
                    let gpio_ident = Ident::new(&format!("GPIO{pin}"), Span::call_site());
                    quote! { Some(esp_hal::gpio::Flex::new($p.#gpio_ident)) }
                }
                _ => quote! { None },
            })
            .collect();

        quote! {
            /// Build a [`wasm_runtime::Pins`] set from the manifest's
            /// `gpios.general_purpose` list, minus any pins claimed by the
            /// Signal Layer (bus pins or `device.pins`). Generated from the board
            /// manifest; firmware calls this in place of
            /// `wasm_runtime::pins_from_peripherals!` when the Signal Layer is
            /// active.
            ///
            /// Each exposed GPIO is moved out of `$p`, so the borrow checker
            /// will reject any later use of `$p.GPIOn` for those pins. Reserved
            /// pins are emitted as `None` and remain available on `$p` for use
            /// by `pipeline_board_peripherals!`. Pins not listed in
            /// `general_purpose` are also `None` — letting a board hide chip
            /// pins that aren't physically broken out.
            #[macro_export]
            macro_rules! pipeline_pins {
                ($p:ident) => {
                    wasm_runtime::Pins([
                        #(#slots),*
                    ])
                };
            }
        }
    }

    fn validate_manifest(
        &self,
        manifest: &pipeline_codegen::manifest::BoardManifest,
    ) -> Vec<pipeline_codegen::validate::ValidationError> {
        use pipeline_codegen::manifest::BusTransport;
        use pipeline_codegen::validate::ValidationError;

        let mut errors = Vec::new();
        for (bus_id, bus) in &manifest.buses {
            match bus.transport {
                BusTransport::I2c => {
                    if bus_id != "i2c0" && bus_id != "i2c1" {
                        errors.push(ValidationError::new(format!(
                            "bus `{bus_id}`: ESP32 exposes I2C0 and I2C1 only; \
                             use id `i2c0` or `i2c1`"
                        )));
                    }
                }
                BusTransport::Spi => {
                    if bus_id != "spi2" && bus_id != "spi3" {
                        errors.push(ValidationError::new(format!(
                            "bus `{bus_id}`: ESP32 user SPI buses are SPI2 and SPI3; \
                             use id `spi2` or `spi3`"
                        )));
                    }
                    // The chip drives the bus pins directly, so the manifest
                    // must wire them (on Linux the spidev node owns them).
                    for required in ["sclk", "mosi"] {
                        if !bus.pins.contains_key(required) {
                            errors.push(ValidationError::new(format!(
                                "bus `{bus_id}`: SPI bus must declare a `{required}` pin"
                            )));
                        }
                    }
                }
            }
        }

        let Some(layout) = chip_pin_layout(&manifest.chip) else {
            errors.push(ValidationError::new(format!(
                "chip `{}` has no pin layout in the ESP backend; supported chips are {}",
                manifest.chip,
                supported_chips()
            )));
            return errors;
        };
        let chip_pins: std::collections::BTreeSet<u8> = layout.into_iter().flatten().collect();
        for &pin in &manifest.gpios.general_purpose {
            if !chip_pins.contains(&pin) {
                errors.push(ValidationError::new(format!(
                    "gpios.general_purpose: GPIO{pin} is not in the {} package layout \
                     (reserved by hardware: flash / USB / strapping / JTAG, or not broken out)",
                    manifest.chip
                )));
            }
        }
        errors
    }
}

/// Chips this backend can generate for: the chip name, its `wasm_runtime::Pins`
/// slot count, and the GPIOs exposed in those slots. A slot's index is the GPIO
/// number, so slots not listed in `usable` are emitted as `None`.
///
/// Each entry must stay in sync with the matching `pins_from_peripherals!` arm in
/// `wasm-runtime/src/imports/gpio.rs`, where the slot count is that arm's
/// `InnerPins` array length. Keeping them in sync is a manual obligation: this is
/// a host crate and `wasm-runtime` only builds for esp-hal targets, so the array
/// lengths are not visible from here and cannot be asserted automatically.
pub(crate) const CHIP_LAYOUTS: &[(&str, u8, &[u8])] = &[
    // GPIO11-15 carry USB-JTAG and strapping; GPIO16-22 carry SPI flash and PSRAM.
    ("esp32c5", 25, &[0, 1, 6, 8, 9, 10, 23, 24]),
    // Available on QFN32 and QFN40 alike. GPIO12/13 are serial-over-USB,
    // GPIO24-26 and 28-30 are SPI flash; GPIO27 doubles as the VDD_SPI pin.
    (
        "esp32c6",
        28,
        &[0, 1, 2, 3, 10, 11, 14, 18, 19, 20, 21, 22, 23, 27],
    ),
    // GPIO3-21 are unavailable; SPI flash and PSRAM occupy 14-17 and 19-21.
    ("esp32c61", 30, &[0, 1, 2, 22, 23, 24, 25, 26, 27, 28, 29]),
];

/// Returns the fixed-size slot layout for `wasm_runtime::Pins` on the given chip,
/// or `None` when the chip has no layout defined.
pub(crate) fn chip_pin_layout(chip: &str) -> Option<Vec<Option<u8>>> {
    let &(_, slots, usable) = CHIP_LAYOUTS.iter().find(|(name, _, _)| *name == chip)?;
    Some(
        (0..slots)
            .map(|pin| usable.contains(&pin).then_some(pin))
            .collect(),
    )
}

/// Comma-separated list of supported chip names, for error messages.
pub(crate) fn supported_chips() -> String {
    CHIP_LAYOUTS
        .iter()
        .map(|(name, _, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn snake_ident(s: &str) -> proc_macro2::Ident {
    let snake = s.replace('-', "_");
    proc_macro2::Ident::new(&snake, proc_macro2::Span::call_site())
}

pub(crate) fn gpio_ty(pin: u8) -> TokenStream {
    let ident = Ident::new(&format!("GPIO{pin}"), Span::call_site());
    quote!(esp_hal::peripherals::#ident<'static>)
}

pub(crate) fn spi_mode_variant(mode: u8) -> TokenStream {
    match mode {
        1 => quote!(SpiMode::Mode1),
        2 => quote!(SpiMode::Mode2),
        3 => quote!(SpiMode::Mode3),
        _ => quote!(SpiMode::Mode0),
    }
}

#[cfg(test)]
mod layout_tests {
    use super::{CHIP_LAYOUTS, chip_pin_layout, supported_chips};

    /// `wasm_runtime::Pins` is indexed by GPIO number, so a slot holding `Some(n)`
    /// must sit at index `n`. Cells address pins by their real number and rely on it.
    #[test]
    fn slot_index_is_the_gpio_number() {
        for (chip, _, _) in CHIP_LAYOUTS {
            let layout = chip_pin_layout(chip).expect("layout exists");
            for (idx, slot) in layout.iter().enumerate() {
                if let Some(pin) = slot {
                    assert_eq!(
                        usize::from(*pin),
                        idx,
                        "{chip}: GPIO{pin} sits in slot {idx}"
                    );
                }
            }
        }
    }

    /// Slot counts match the `InnerPins` array lengths in
    /// `wasm-runtime/src/imports/gpio.rs`. Nothing enforces this across crates,
    /// so pin them down here to catch an accidental truncation.
    #[test]
    fn slot_counts_are_the_documented_lengths() {
        for (chip, slots, _) in CHIP_LAYOUTS {
            let layout = chip_pin_layout(chip).expect("layout exists");
            assert_eq!(layout.len(), usize::from(*slots), "{chip} slot count");
        }
        let counts: Vec<_> = CHIP_LAYOUTS.iter().map(|(c, s, _)| (*c, *s)).collect();
        assert_eq!(
            counts,
            vec![("esp32c5", 25), ("esp32c6", 28), ("esp32c61", 30)]
        );
    }

    #[test]
    fn exposed_pins_match_the_runtime_arms() {
        let exposed = |chip: &str| -> Vec<u8> {
            chip_pin_layout(chip)
                .expect("layout exists")
                .into_iter()
                .flatten()
                .collect()
        };
        assert_eq!(exposed("esp32c5"), vec![0, 1, 6, 8, 9, 10, 23, 24]);
        assert_eq!(
            exposed("esp32c6"),
            vec![0, 1, 2, 3, 10, 11, 14, 18, 19, 20, 21, 22, 23, 27]
        );
        assert_eq!(
            exposed("esp32c61"),
            vec![0, 1, 2, 22, 23, 24, 25, 26, 27, 28, 29]
        );
    }

    /// The pins carrying SPI flash and PSRAM must never be exposed: the chip is
    /// executing out of that flash, so driving them stops the firmware.
    #[test]
    fn flash_and_psram_pins_are_never_exposed() {
        for (chip, reserved) in [
            ("esp32c5", &[16u8, 17, 18, 19, 20, 21, 22][..]),
            ("esp32c6", &[24, 25, 26, 28, 29, 30][..]),
            ("esp32c61", &[14, 15, 16, 17, 19, 20, 21][..]),
        ] {
            let exposed: Vec<u8> = chip_pin_layout(chip)
                .expect("layout exists")
                .into_iter()
                .flatten()
                .collect();
            for pin in reserved {
                assert!(
                    !exposed.contains(pin),
                    "{chip}: GPIO{pin} carries flash or PSRAM and must not be exposed"
                );
            }
        }
    }

    #[test]
    fn unknown_chip_has_no_layout() {
        assert!(chip_pin_layout("esp32p4").is_none());
        assert!(chip_pin_layout("").is_none());
    }

    #[test]
    fn supported_chips_lists_every_layout() {
        let listed = supported_chips();
        for (chip, _, _) in CHIP_LAYOUTS {
            assert!(listed.contains(chip), "{chip} missing from {listed}");
        }
    }
}
