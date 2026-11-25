# Part 2: A driver

A driver knows one device: how to wake it, and how to ask it for a reading. Everything else in the
Signal Layer is generic, so supporting a new sensor means writing a driver and nothing else. This
part writes one for an I2C temperature sensor.

## Scaffold it

A driver is tied to a bus, and you name it explicitly: `--bus i2c` or `--bus spi`. There is no
default, because the two are different code.

```bash
myrmic signal-layer driver thermo --bus i2c --registry-dir ~/sl-modules
```

This lands `thermo` next to the deadband step, under `drivers/` this time:

```text
~/sl-modules/
    drivers/thermo/{Cargo.toml, descriptor.yaml, src/lib.rs}
    steps/deadband/...
```

As before, the scaffold compiles and its tests pass out of the box:

```bash
cd ~/sl-modules/drivers/thermo
cargo test
```

## The rule that shapes a driver

A driver never knows where it is: not which pin, not which board, not whether it runs on an ESP32
or a Linux box. It is handed the bus on every call and gets on with it. That is enforced by the
shape of the code: the driver is generic over the bus using the `embedded-hal` traits, so it
physically cannot reach for a specific pin or chip. The same crate then works on every platform
with no conditionals.

## What the scaffold gives you

The generated `src/lib.rs` is a complete, illustrative I2C driver you edit into your own. Its shape:

- A **`ThermoConfig`**: one field per `config_schema` entry. A `scope: hardware` field (like an I2C
  address) is a fact about the wiring and comes from the board file; a `scope: application` field is
  a knob the pipeline sets.
- A **`ThermoReadings`**: one field per `outputs` entry in the descriptor, named and typed to match.
  Codegen wires `readings.temperature` to whatever the pipeline calls it, so the names must line up.
- A **`ThermoError<E>`**: a `Bus(E)` variant that carries the bus error, plus a domain variant.
  The `From<E>` impl lets `?` lift a bus error.
- **`new` / `init` / `sample`**: `new` is infallible and touches no bus; `init` verifies the sensor
  answers with the identity it should and configures it; `sample` reads one measurement. `init` is
  idempotent, because the runtime re-runs it to recover a sensor after a bus glitch.

## Fill it in

Replace `src/lib.rs` with the sensor you are actually writing. This one reads a signed 16-bit
temperature in units of 1/256 C from one register, and checks a `WHO_AM_I` register during `init`
so a mis-wired bus fails with a clear error instead of nonsense readings:

```rust
#![cfg_attr(not(test), no_std)]
//! thermo: a minimal, illustrative I2C temperature sensor driver.

use embedded_hal_async::i2c::I2c;

const REG_WHO_AM_I: u8 = 0x0F;
const WHO_AM_I: u8 = 0xBA;
const REG_CTRL: u8 = 0x20;
const CTRL_ENABLE: u8 = 0x01;
const REG_TEMP: u8 = 0x00;

pub struct ThermoConfig {
    pub i2c_addr: u8,
}

impl Default for ThermoConfig {
    fn default() -> Self {
        Self { i2c_addr: 0x48 }
    }
}

#[non_exhaustive]
pub struct ThermoReadings {
    pub temperature: f32,
}

#[non_exhaustive]
#[derive(Debug)]
pub enum ThermoError<E: core::fmt::Debug> {
    Bus(E),
    InvalidId(u8),
}

impl<E: core::fmt::Debug> From<E> for ThermoError<E> {
    fn from(err: E) -> Self {
        Self::Bus(err)
    }
}

pub struct Thermo {
    cfg: ThermoConfig,
}

impl Thermo {
    #[must_use]
    pub fn new(cfg: &ThermoConfig) -> Self {
        Self { cfg: ThermoConfig { i2c_addr: cfg.i2c_addr } }
    }

    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), ThermoError<I::Error>> {
        let mut id = [0u8; 1];
        bus.write_read(self.cfg.i2c_addr, &[REG_WHO_AM_I], &mut id).await?;
        if id[0] != WHO_AM_I {
            return Err(ThermoError::InvalidId(id[0]));
        }
        bus.write(self.cfg.i2c_addr, &[REG_CTRL, CTRL_ENABLE]).await?;
        Ok(())
    }

    pub async fn sample<I: I2c>(&mut self, bus: &mut I) -> Result<ThermoReadings, ThermoError<I::Error>> {
        let mut raw = [0u8; 2];
        bus.write_read(self.cfg.i2c_addr, &[REG_TEMP], &mut raw).await?;
        let temperature = f32::from(i16::from_be_bytes(raw)) / 256.0;
        Ok(ThermoReadings { temperature })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};
    use futures::executor::block_on;

    const ADDR: u8 = 0x48;

    #[test]
    fn init_and_sample() {
        // 25.0 C is 25.0 * 256 = 6400 = 0x1900.
        let mut mock = Mock::new(&[
            T::write_read(ADDR, vec![REG_WHO_AM_I], vec![WHO_AM_I]),
            T::write(ADDR, vec![REG_CTRL, CTRL_ENABLE]),
            T::write_read(ADDR, vec![REG_TEMP], vec![0x19, 0x00]),
        ]);
        let mut dev = Thermo::new(&ThermoConfig::default());
        block_on(async {
            dev.init(&mut mock).await.unwrap();
            let reading = dev.sample(&mut mock).await.unwrap();
            assert_eq!(reading.temperature, 25.0);
        });
        mock.done();
    }

    #[test]
    fn wrong_chip_id_returns_error() {
        let mut mock = Mock::new(&[T::write_read(ADDR, vec![REG_WHO_AM_I], vec![0x00])]);
        let mut dev = Thermo::new(&ThermoConfig::default());
        block_on(async {
            let err = dev.init(&mut mock).await.unwrap_err();
            assert!(matches!(err, ThermoError::InvalidId(0x00)));
        });
        mock.done();
    }
}
```

The tests are the ones that catch what matters. You supply the raw register bytes, so you can drive
the decoding path with numbers you control: a raw sample in, an assertion about the temperature out,
and a check that a wrong identity is reported as an error rather than decoded as a reading. Whether
the wiring is right is a question for the bench; whether your maths is right is one you answer here.

## Match the descriptor

Update `descriptor.yaml` to the one output and the one hardware knob this sensor has:

```yaml
id: thermo
category: sensor-drivers
description: >
  Illustrative I2C temperature sensor: one temperature reading in degrees C.

requires:
  buses:
    - transport: i2c

outputs:
  - name: temperature
    type: f32
    unit: "°C"

config_schema:
  i2c_addr:
    scope: hardware
    rust_type: u8
    default: 0x48
    description: "I2C address"
  sample_interval_ms:
    scope: application
    rust_type: u64
    default: 1000
    description: Polling interval in milliseconds
```

`sample_interval_ms` gets no `ThermoConfig` field: it drives the generated sampling loop, which
ticks at that rate, and is never handed to the driver. Every other `config_schema` entry needs a
matching field.

```bash
cargo test
```

An SPI sensor is the same shape with `--bus spi`: the driver is generic over the `SpiDevice`
trait instead, and the descriptor declares `transport: spi`. [Next](03_using-them.md), you point a
pipeline at your registry and run both modules.
