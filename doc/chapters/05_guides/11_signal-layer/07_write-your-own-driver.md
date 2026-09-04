# Write your own driver

A driver is the piece that knows one kind of device, a sensor or an actuator: how to wake it,
how to ask it for a reading, or how to make it act. Everything else in the Signal Layer is
generic, so supporting a new sensor or a new actuator means writing a driver and nothing more.

The work happens in a checkout of the `myrmic` repository. A driver is a small Rust crate, a
directory under `signal-modules/drivers/` holding `Cargo.toml`, `src/lib.rs` with the logic, and
a `descriptor.yaml` telling the generator what it needs and what it produces; the shipped drivers
around it are working examples of everything this page describes.

Start `src/lib.rs` with `#![cfg_attr(not(test), no_std)]`. Every shipped driver has it, and without
it your crate builds on your laptop and fails the moment someone compiles it for a board.

## The rule that shapes everything

A driver never knows where it is.

It does not know which pin it is on, which board it is running on, or whether it sits on an
ESP32 or a Linux box. It is handed what it needs and gets on with it, which is why the same
driver crate works on every supported platform without a single conditional.

That is not a style preference. It is enforced by the shape of the code: your driver is generic
over the hardware it touches, using the `embedded-hal` traits, so it physically cannot reach for
a specific pin or a specific chip.

Everything below follows from that one idea.

## Sensor drivers: producing values

A sensor driver is handed the bus on every call and does not keep it. This is deliberate: a bus
is shared between every device on it, so no single driver can own it.

```rust
// The register map of your part.
const REG_CHIP_ID: u8 = 0xD0;
const REG_CTRL: u8 = 0xF4;
const REG_VALUE: u8 = 0xFA;
const CHIP_ID: u8 = 0x60;

pub enum MySensorError<E> {
    /// The bus itself failed; carries the underlying error.
    Bus(E),
    /// A device answered, but it is not the part we expected.
    WrongChip { found: u8 },
}

/// One field per descriptor `outputs` entry, named and typed to match.
pub struct MySensorReadings {
    pub temperature: f32,
}

pub struct MySensor { cfg: MySensorConfig }

impl MySensor {
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), MySensorError<I::Error>> {
        // Confirm we are talking to the right part before configuring it.
        let mut id = [0u8; 1];
        bus.write_read(self.cfg.i2c_addr, &[REG_CHIP_ID], &mut id)
            .await
            .map_err(MySensorError::Bus)?;
        if id[0] != CHIP_ID {
            return Err(MySensorError::WrongChip { found: id[0] });
        }
        // One-time configuration: measurement mode, filters, whatever your part needs.
        bus.write(self.cfg.i2c_addr, &[REG_CTRL, 0b0000_0011])
            .await
            .map_err(MySensorError::Bus)
    }

    pub async fn sample<I: I2c>(
        &mut self,
        bus: &mut I,
    ) -> Result<MySensorReadings, MySensorError<I::Error>> {
        // Read the measurement registers in one transaction.
        let mut raw = [0u8; 2];
        bus.write_read(self.cfg.i2c_addr, &[REG_VALUE], &mut raw)
            .await
            .map_err(MySensorError::Bus)?;
        // Convert raw counts into the unit the descriptor promises (°C here).
        let temperature = f32::from(i16::from_be_bytes(raw)) / 100.0;
        Ok(MySensorReadings { temperature })
    }
}
```

The `cfg` field is the driver's configuration; the next section shows the descriptor it comes
from and the struct it fills.

Three things to copy from that shape.

**The bus is a generic parameter on the method**, not a field on the struct, and it comes from
`embedded_hal_async`. Sensor work is asynchronous because a bus transaction takes real time and
the runtime has other things to do meanwhile.

**The readings are a struct whose fields match the descriptor's `outputs`.** The generator wires
`readings.temperature` to whatever the pipeline called it, so the names have to line up.

**The error type carries the bus error rather than swallowing it.** A `Bus(E)` variant plus your
own domain variants is the pattern, and the `WrongChip` variant above is the one worth copying:
checking an identity register during `init` and failing distinctly is what turns a mis-wired
sensor into a clear message instead of nonsense readings. The shipped `bme280` driver does
exactly this.

`init` runs before the first sample and again whenever the pipeline is trying to recover a failed
source, so it must be safe to call more than once.

### The sensor's descriptor

The generator never sees your Rust source. What it reads is the `descriptor.yaml` next to it
(the shipped descriptors under `signal-modules/` are the reference; this one is a minimal
sensor's):

```yaml
id: my-sensor
category: sensor-drivers
description: >
  What the device is and what it measures.
requires:
  buses:
    - transport: i2c
config_schema:
  i2c_addr:
    scope: hardware
    rust_type: u8
    default: 0x76
    description: "I2C address (SDO to GND = 0x76, SDO to VCC = 0x77)"
  sample_interval_ms:
    scope: application
    rust_type: u64
    default: 1000
    description: Polling interval in milliseconds
outputs:
  - name: temperature
    type: f32
    unit: "°C"
```

`requires` says what the device needs to be reachable: a bus transport, and optionally named
pins the board file will assign. `outputs` names what a sensor produces, and those names are
what a pipeline points a tap at. An actuator declares `writes` instead, which the next section
covers.

`config_schema` is where a driver states its knobs, and the `scope` on each one is load-bearing.
`hardware` means the value is a fact about the wiring, so it belongs in the board file.
`application` means it can change without touching a wire, so it belongs in the pipeline. Pick
the wrong one and the generator rejects the config rather than quietly accepting it in the wrong
place, so this is worth getting right the first time.

The `cfg` in the sensor snippet above is where these fields land. `MySensorConfig` is not
generated; you define it in the driver crate, one field per `config_schema` entry, and the
generator emits the code that fills it: board-file values for `hardware` scope, pipeline values
for `application` scope, and the descriptor's defaults for whatever neither sets. For the
descriptor above, the matching struct is:

```rust
/// One field per `config_schema` entry, named and typed to match.
pub struct MySensorConfig {
    /// I2C address: 0x76 with SDO to GND, 0x77 with SDO to VCC.
    pub i2c_addr: u8,
}
```

One field is deliberately missing. `sample_interval_ms` is consumed by the generated sampling
loop itself, which ticks at that rate; it is never handed to the driver, so it gets no struct
field. Every other `config_schema` entry needs one.

## Actuator drivers: consuming outlet values

An actuator driver is the mirror image, and the differences are not arbitrary.

```rust
pub struct MyActuator<P> {
    pin: P,
    cfg: MyActuatorConfig,
    is_on: bool,
    last_switch_ms: u64,
}

impl<P: OutputPin> MyActuator<P> {
    pub fn init(&mut self) -> Result<(), MyActuatorError<P::Error>> {
        // Drive the pin to a known-safe state before the first write.
        self.pin.set_low().map_err(MyActuatorError::Pin)
    }

    pub fn apply(
        &mut self,
        cmd: DigitalState,
        now_ms: u64,
    ) -> Result<(), MyActuatorError<P::Error>> {
        // Protect the hardware: ignore writes arriving faster than it can stand.
        if now_ms.saturating_sub(self.last_switch_ms) < self.cfg.min_switch_interval_ms {
            return Ok(());
        }
        // A no-op write is not a switch; don't burn a relay cycle on it.
        if cmd.on == self.is_on {
            return Ok(());
        }
        if cmd.on { self.pin.set_high() } else { self.pin.set_low() }
            .map_err(MyActuatorError::Pin)?;
        self.is_on = cmd.on;
        self.last_switch_ms = now_ms;
        Ok(())
    }
}
```

**It owns its hardware.** A pin belongs to exactly one device, so the driver takes it and keeps
it, which is the opposite of the bus case and for the same reason.

**It is synchronous.** Setting a pin or a duty cycle does not wait for anything.

**It is given the time.** `apply` receives `now_ms` rather than reading a clock, so the driver
stays testable and the runtime keeps control of what time means.

### The actuator's descriptor

The descriptor declares `writes` instead of `outputs`:

```yaml
writes:
  type: DigitalState
  mode: digital
requires:
  optional_pins: [out]
```

`type` is the value the driver accepts and is checked against the outlet that drives it.
`mode` tells the generator what kind of hardware to hand over: a plain output for `digital`, a
configured PWM channel for `pwm` (pulse-width modulation: a fixed-frequency square wave whose
on-fraction the value sets, which is how fan speeds and LED brightness are driven).

### Protect the hardware inside the driver

This is the part worth taking seriously.

The pipeline re-applies a write whenever a new one arrives, and a cell may write far more often
than a device can stand. Nothing upstream knows what your relay can survive, so the limit has to
live in the driver.

The built-in output drivers can do this: they refuse to act again within a minimum interval,
and the digital one additionally ignores a write asking for the state it is already in. But
the interval defaults to zero, meaning no limit, so it protects nothing until a board file sets
it. What the design does guarantee is *where* the setting lives: it is `hardware` scope, so a
pipeline cannot weaken it.

If your device has a physical limit, encode it the same way: as a `hardware`-scope config field
with a sane default, enforced inside `apply`. A driver that faithfully relays every write to a
relay rated for a few hundred thousand operations is not being neutral, it is being negligent.

### Reporting real state

A driver whose hardware can report back can expose that too. Add `read_status` and declare the
fields as outputs, and the pipeline publishes them as taps alongside the outlet.

The rule here is that status comes only from a genuine read. Never infer it from the value you
just wrote: the entire value of a feedback line is that it disagrees with you when something has
gone wrong.

## The naming contract

The naming is a contract, and it runs through the **directory**, not the `id:` field. A board file
naming `driver: bme280` makes the generator look in `bme280/` for the descriptor and add a
dependency on the crate `bme280-driver`. Keep all three aligned. The `id:` key inside the
descriptor is not read at all, so correcting that alone fixes nothing.

## Configuration and defaults

Every `config_schema` field needs a default that works, because a board file or pipeline may not
mention it. Prefer defaults matching the most common wiring of the part.

For a knob with a fixed set of choices, use a typed enum rather than a raw number. The generator
maps it, the pipeline reads legibly, and an invalid value is caught during generation rather than
producing a device in a strange mode.

## Testing without hardware

Nothing in this section goes through the Signal Layer. A driver is a standalone Rust crate, so
its tests are ordinary `cargo test` unit tests: no pipeline, no board file, no generator. That is
worth saying because it is easy to assume the fake hardware has to be injected from somewhere
above, and it does not.

The generic parameter *is* the injection point, and where it goes follows the same ownership
split as before.

An actuator owns its hardware, so the fake goes in at construction:

```rust
struct FakePin { high: bool }

impl embedded_hal::digital::ErrorType for FakePin {
    type Error = core::convert::Infallible;
}

impl embedded_hal::digital::OutputPin for FakePin {
    fn set_high(&mut self) -> Result<(), Self::Error> { self.high = true; Ok(()) }
    fn set_low(&mut self)  -> Result<(), Self::Error> { self.high = false; Ok(()) }
}

let mut out = MyActuator::new(&MyActuatorConfig::default(), FakePin { high: false });
out.apply(DigitalState { on: true }, 0).unwrap();
assert!(out.pin.high);
```

A sensor borrows its bus, so the fake goes in per call. Here `embedded-hal-mock` does the work:
you declare the transactions you expect and hand the mock to the driver.

```rust
use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};

let mut mock = Mock::new(&txns);              // canned register contents
driver.init(&mut mock).await.unwrap();
let readings = driver.sample(&mut mock).await.unwrap();
```

That second form is more useful than it first looks. Because you supply the raw register bytes,
you can drive the decoding path with numbers you control: calibration constants from the
datasheet, a raw sample, and an assertion about what comes out. The BME280 driver tests this
way, including a test that a wrong chip identity is reported as an error rather than decoded as
a reading.

These are the tests that catch the errors that matter. Whether the wiring is right is a question
for the bench; whether your maths is right is a question you can answer at your desk.

## Adding it to the tree

The repository is one Cargo workspace, and `signal-modules/drivers/*` is a workspace-member glob, so a driver crate placed alongside the others is picked up automatically.
It also needs an entry in the root `[workspace.dependencies]`, because the generated pipeline
crate refers to it as a workspace dependency; without that, generation emits a `Cargo.toml`
that does not resolve. Once that is in place, a board file can name the driver on a device and
a pipeline can point at it.

From there, the [board file](./01_describe-your-hardware.md) decides where the device is wired
and the [pipeline](./02_design-your-pipeline.md) decides what happens to its values.
