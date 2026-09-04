# Agent prompt: create a sensor driver from a datasheet

Use this as the prompt when asking an AI agent to implement a new driver.
The human workflow (workspace registration, board manifest, pipeline YAML) is
covered in the driver guide in the handbook; this prompt covers only the driver crate itself.

---

```
You are implementing a new sensor driver for the swarm Signal Layer.
The driver must follow the exact conventions of the existing drivers in
`signal-modules/drivers/`. Read bme280 (no optional pins) and ccs811
(optional pins) in full before writing any code — they are the canonical
references.

Reference files to read first:
  signal-modules/drivers/bme280/src/lib.rs
  signal-modules/drivers/bme280/descriptor.yaml
  signal-modules/drivers/bme280/Cargo.toml
  signal-modules/drivers/ccs811/src/lib.rs
  signal-modules/drivers/ccs811/descriptor.yaml

---

## Inputs

- Sensor name (crate-id form, e.g. `sht41`): <SENSOR_ID>
- Datasheet: <PATH_OR_URL>
- Outputs to expose: <ALL or a subset, e.g. "temperature and humidity only">
  If omitted, expose every measurable quantity the sensor produces.

Read the datasheet before writing a single line of code. Extract from it:
- The bus interface the sensor uses (I2C, SPI, or both — pick one)
- The chip ID register and expected value (for `init` verification)
- All register addresses needed for init, configuration, and reading
- If I2C: the default address and any address-select pin options
- If SPI: the max clock speed, CPOL/CPHA mode, and any chip-select timing constraints
- Any optional GPIO pins (interrupt, reset, enable) — their names, active
  levels, and what the driver gains by wiring them
- Startup timing requirements (reset pulse width, boot delay, etc.)
- The raw-to-physical compensation formula for each output

---

## What to create

Create one directory: `signal-modules/drivers/<SENSOR_ID>/`
with exactly three files.

### 1. `Cargo.toml`

Follow bme280-driver verbatim:
- package.name = `<SENSOR_ID>-driver`
- workspace keys for version/edition/authors/publish
- [dependencies]: embedded-hal-async + log (no-default-features)
- [dev-dependencies]: embedded-hal-mock (features = ["embedded-hal-async"]) + futures (features = ["executor"])
- [lints] workspace = true

### 2. `src/lib.rs`

Rules (non-negotiable):
- `#![cfg_attr(not(test), no_std)]` as the first line
- No heap, no std, no external sensor crates — implement the protocol directly from the datasheet
- All register addresses and bitmasks as top-level `const` items with comments citing the datasheet section
- Sensor startup delays and reset waits gated with `#[cfg(not(test))]` using
  `embassy_time::Timer::after_millis()`; interrupt timeouts use `embassy_time::with_timeout()`
  the same way
- `log::debug!("[<sensor_id>] ...")` at the end of every successful `sample()`
- `log::info!("[<sensor_id>] init OK")` at the end of successful `init()`
- Split construction (`new`, infallible, no bus) from bring-up (`init(&mut self, bus)`,
  fallible, **re-runnable** for recovery). `new` zero-initialises any chip state `init` loads;
  `init` must be safe to call repeatedly (the generated source task re-runs it to recover a
  degraded sensor). `sample` is only valid after a successful `init`.

**Bus trait to import** — determined by the sensor's primary interface:

| Bus  | Import | Bus parameter type |
|------|--------|--------------------|
| I2C  | `embedded_hal_async::i2c::I2c` | `&mut I` where `I: I2c` |
| SPI  | `embedded_hal_async::spi::SpiDevice` | `&mut S` where `S: SpiDevice` |

For I2C, also add `embedded_hal_async::digital::Wait` if the sensor has optional interrupt pins.
For SPI, chip-select is managed by the bus layer — the driver never touches a CS pin directly.

Public API surface — follow this shape, substituting the correct bus trait:

```rust
// I2C variant:
pub struct <Sensor>Config { pub i2c_addr: u8 }
impl Default for <Sensor>Config { fn default() -> Self { Self { i2c_addr: <ADDR> } } }

// SPI variant — no address in config (CS is wired in the board manifest):
pub struct <Sensor>Config { /* sensor-specific config only, e.g. full_scale */ }
impl Default for <Sensor>Config { ... }

pub struct <Sensor>Readings { pub <field>: f32, ... }

#[derive(Debug)]
pub enum <Sensor>Error<E: core::fmt::Debug> {
    Bus(E),
    InvalidId(u8),
    // add sensor-specific variants as needed (e.g. NotReady, SensorError)
}
impl<E: core::fmt::Debug> From<E> for <Sensor>Error<E> {
    fn from(e: E) -> Self { Self::Bus(e) }
}

pub struct <Sensor> { /* state: stored Config, calibration (zeroed by `new`), mode flags, etc. */ }

impl <Sensor> {
    // I2C:
    pub fn new(cfg: &<Sensor>Config) -> Self { ... }                 // infallible, no bus access
    pub async fn init<I: I2c>(&mut self, bus: &mut I)
        -> Result<(), <Sensor>Error<I::Error>> { ... }               // fallible, re-runnable
    pub async fn sample<I: I2c>(&mut self, bus: &mut I)
        -> Result<<Sensor>Readings, <Sensor>Error<I::Error>> { ... }

    // SPI:
    pub fn new(cfg: &<Sensor>Config) -> Self { ... }
    pub async fn init<S: SpiDevice>(&mut self, dev: &mut S)
        -> Result<(), <Sensor>Error<S::Error>> { ... }
    pub async fn sample<S: SpiDevice>(&mut self, dev: &mut S)
        -> Result<<Sensor>Readings, <Sensor>Error<S::Error>> { ... }
}
```

If the sensor has optional GPIO pins, follow ccs811 exactly:
- `pub struct NoPin;` — marker type for the "no pins wired" case.
- `NoPin` must implement `embedded_hal::digital::ErrorType` and
  `embedded_hal_async::digital::Wait` as never-resolving stubs
  (`core::future::pending().await; unreachable!()`). This lets a single generic
  `sample()` impl serve both `Ccs811<NoPin>` and `Ccs811<RealPin>` without a
  duplicate method body — the polling path is selected purely by the runtime
  `Option<P>` field rather than by the type.
- `pub struct <Sensor>Pins<P> { pub <pin_name>: Option<P>, ... }`
- `impl Default for <Sensor>Pins<P>`
- `pub struct <Sensor><P = NoPin>` storing `Option<P>` per pin — the pin is moved in at
  construction and **owned for the instance's lifetime**, so re-running `init` for recovery
  never needs the pin re-passed.
- `impl <Sensor><NoPin>` with `new()` (polling-only construction).
- `impl<P: embedded_hal_async::digital::Wait> <Sensor><P>` with `new_with_pins()` (stores the
  pins), a re-runnable `init(&mut self, bus)`, and a single `sample()` — when `Option<P>` is
  `Some`, `wait_for_low()` is awaited first; when `None`, it falls straight through to a
  STATUS-register poll.

### 3. `descriptor.yaml`

```yaml
id: <sensor_id>
category: sensor-drivers
description: >
  <Manufacturer> <PartNumber> — <what it measures> over <I2C|SPI>.
requires:
  buses:
    - transport: <i2c|spi>   # match the bus the driver uses
  # only include if the driver has optional GPIO pins:
  optional_pins:
    - <pin_name>
outputs:
  - name: <field_name>    # must match the Readings struct field name exactly
    type: f32
    unit: "<SI unit>"
  # repeat for each output
config_schema:
  # I2C only — omit for SPI:
  i2c_addr:
    scope: hardware
    rust_type: u8
    default: <0xNN>
    description: "I2C address (<addr_pin>=low=<hex>, <addr_pin>=high=<hex>)"
  sample_interval_ms:
    scope: application
    rust_type: u64
    default: 1000
    description: Polling interval in milliseconds
  # add any other sensor-specific hardware config fields as needed
```

**Typed enum knobs:** when a config field is categorical (oversampling level, data rate,
gain, …), define a `#[repr(u8)]` enum in `src/lib.rs` and use the enum name as `rust_type`
and the exact variant name as `default`. Add a comment listing variants for pipeline authors:

```yaml
  data_rate:
    scope: application
    rust_type: DataRate
    default: Sps128
    # Variants: Sps8 | Sps16 | Sps32 | Sps64 | Sps128 | Sps250 | Sps475 | Sps860
    description: "Conversion data rate — higher rates are faster but noisier."
```

Pipeline YAML then references the variant by its exact Rust name (`data_rate: Sps860`).
Codegen emits the qualified path (`my_driver::DataRate::Sps860`); a stale or typo'd name
is a compile error. Use a primitive type (`u8`, `u32`) only for true numeric quantities
(millivolt full-scale range, I²C address) where any value in range is valid.

---

## Tests (required)

Write a `#[cfg(test)] mod tests` block in `src/lib.rs`.

**For I2C drivers** use `embedded_hal_mock::eh1::i2c::{Mock, Transaction as T}`.
**For SPI drivers** use `embedded_hal_mock::eh1::spi::{Mock, Transaction as T}`.
Wrap all async tests in `futures::executor::block_on(async { ... })`.

Required test cases:
1. `init_and_sample` — happy path: mock the exact byte sequences from the
   datasheet (real register addresses, realistic response values), construct with
   `new()`, call `init()` then `sample()`, assert readings are in physically plausible
   ranges, call `mock.done()`.
2. `wrong_chip_id_returns_error` — `new()` then `init()` against a mocked bad ID byte,
   assert `InvalidId`.
3. `reinit_recovers_after_first_bring_up` — mock **two** full bring-up sequences
   back-to-back, call `init()` twice on the same instance (the recovery path), then
   `sample()` and assert it still works. Proves `init` is re-runnable.
4. Any sensor-specific error paths visible in the status register (e.g.
   `not_ready`, `sensor_error`) — one test per variant, model after ccs811.
5. If optional pins:
   - `init_with_pins_none_falls_back_to_polling` — `new_with_pins` with `Pins { pin: None }`
     must select polling mode at `init` and check the status register at sample time.
   - `interrupt_driven_init_and_sample` — `new_with_pins` with `Pins { pin: Some(...) }` must
     select interrupt mode; include the `ImmediateLow` / `ImmediateHigh` helper struct
     that implements `embedded_hal_async::digital::Wait`.

Mock bytes must come from the datasheet. Do not use all-zero payloads unless
the sensor genuinely returns zeros for that measurement.

---

## After creating the files

1. Run `cargo test -p <sensor_id>-driver` and fix all compilation errors and
   test failures before reporting done.
2. Confirm that every `outputs[].name` in `descriptor.yaml` exactly matches a
   field name in the `<Sensor>Readings` struct.
3. Do NOT add the crate to any Cargo workspace `members` list, `workspace.dependencies`,
   board manifest, or pipeline file — that is done separately by the user
   (see the driver guide in the handbook).
```
