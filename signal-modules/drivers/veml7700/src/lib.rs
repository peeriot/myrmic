//! Vishay VEML7700 ambient light sensor driver.
//!
//! High-accuracy ambient light sensor over I2C at the fixed address `0x10`.
//! Returns illuminance in lux, derived from the raw 16-bit ALS count and a
//! per-configuration resolution (LSB) computed from gain × integration time.
//!
//! # Configuration
//!
//! [`Veml7700Config`] exposes the two datasheet knobs that change the lux
//! scale:
//!
//! - **Gain** (`gain`) — see [`Gain`]. Lower gain raises the measurable range
//!   (and the LSB); higher gain improves resolution at low light levels.
//! - **Integration time** (`integration_time`) — see [`IntegrationTime`].
//!   Longer integration averages photon noise and lowers the LSB at the cost
//!   of measurement latency.
//!
//! The driver computes the lux LSB from these two fields at [`Veml7700::init`]
//! and stores it; [`Veml7700::sample`] multiplies the raw count by the stored LSB.
//!
//! Defaults: gain ×1, integration 100 ms (matches the datasheet's typical
//! default profile).
//!
//! # Timing
//!
//! After [`Veml7700::init`] the datasheet requires at least 2× the
//! integration time before the first reading is valid (so 200 ms for the
//! default 100 ms IT). The driver does **not** insert this delay — the
//! caller (typically the codegen-emitted source task with a 1000+ ms
//! `sample_interval_ms`) is expected to wait long enough.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

const REG_ALS_CONF: u8 = 0x00;
const REG_ALS: u8 = 0x04;

/// Amplifier gain — controls the measurable range and per-count resolution.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gain {
    /// ×1 — wide range, moderate resolution.
    X1 = 0,
    /// ×2 — narrower range, highest resolution.
    X2 = 1,
    /// ×1/8 — widest range (bright outdoor light).
    OneEighth = 2,
    /// ×1/4.
    OneFourth = 3,
}

/// ALS integration time — controls measurement latency and per-count resolution.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationTime {
    /// 25 ms — shortest, lowest resolution.
    Ms25 = 0,
    /// 50 ms.
    Ms50 = 1,
    /// 100 ms (default).
    Ms100 = 2,
    /// 200 ms.
    Ms200 = 3,
    /// 400 ms.
    Ms400 = 4,
    /// 800 ms — longest, highest resolution.
    Ms800 = 5,
}

/// Driver configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Veml7700Config {
    /// I2C address. The VEML7700 has a fixed address (`0x10`); the field
    /// exists for symmetry with other I2C drivers in the Signal Layer.
    pub i2c_addr: u8,
    /// Amplifier gain. Affects both the maximum measurable lux and the
    /// per-count resolution.
    pub gain: Gain,
    /// ALS integration time. Affects both measurement latency and the
    /// per-count resolution.
    pub integration_time: IntegrationTime,
}

impl Default for Veml7700Config {
    /// Address `0x10`, gain ×1, integration 100 ms — Vishay's reference
    /// "general indoor" profile.
    fn default() -> Self {
        Self {
            i2c_addr: 0x10,
            gain: Gain::X1,
            integration_time: IntegrationTime::Ms100,
        }
    }
}

/// One ambient-light reading.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Veml7700Readings {
    /// Illuminance in lux. Resolution depends on `gain` × `integration_time`.
    pub lux: f32,
}

/// Errors returned by the VEML7700 driver.
#[non_exhaustive]
#[derive(Debug)]
pub enum Veml7700Error<E: core::fmt::Debug> {
    /// Underlying I2C bus error.
    Bus(E),
}

impl<E: core::fmt::Debug> From<E> for Veml7700Error<E> {
    fn from(e: E) -> Self {
        Self::Bus(e)
    }
}

/// VEML7700 driver instance.
pub struct Veml7700 {
    cfg: Veml7700Config,
    lsb_lux: f32,
}

impl Veml7700 {
    /// Construct a driver instance without touching the bus.
    ///
    /// The lux LSB for the configured gain × integration time is computed here;
    /// the sensor itself is **not** powered on until [`Veml7700::init`] is
    /// called, which must happen before [`Veml7700::sample`].
    #[must_use]
    pub fn new(cfg: &Veml7700Config) -> Self {
        Self {
            cfg: *cfg,
            lsb_lux: lsb_lux(cfg.gain, cfg.integration_time),
        }
    }

    /// (Re-)initialise the sensor: power it on and apply the configured
    /// gain / integration time.
    ///
    /// Safe to call repeatedly — the generated source task re-runs `init` to
    /// recover a sensor that started failing.
    ///
    /// # Errors
    ///
    /// [`Veml7700Error::Bus`] on any I2C transaction failure.
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), Veml7700Error<I::Error>> {
        let addr = self.cfg.i2c_addr;

        // ALS_CONF (16-bit, LE on the wire):
        //   bits 12:11 = gain, bits 9:6 = IT, all others 0 (power on, no IRQ, persistence 1).
        let conf = (u16::from(gain_bits(self.cfg.gain)) << 11)
            | (u16::from(it_bits(self.cfg.integration_time)) << 6);
        let conf_le = conf.to_le_bytes();
        bus.write(addr, &[REG_ALS_CONF, conf_le[0], conf_le[1]])
            .await?;

        log::info!("[veml7700] init OK at 0x{addr:02X}");
        Ok(())
    }

    /// Read one ambient-light sample. Multiplies the raw 16-bit ALS count by
    /// the stored LSB to produce lux.
    ///
    /// # Errors
    ///
    /// [`Veml7700Error::Bus`] on any I2C transaction failure.
    pub async fn sample<I: I2c>(
        &mut self,
        bus: &mut I,
    ) -> Result<Veml7700Readings, Veml7700Error<I::Error>> {
        let mut raw = [0u8; 2];
        bus.write_read(self.cfg.i2c_addr, &[REG_ALS], &mut raw)
            .await?;
        let counts = u16::from_le_bytes(raw);
        let lux = f32::from(counts) * self.lsb_lux;
        log::debug!("[veml7700] {lux:.1} lux");
        Ok(Veml7700Readings { lux })
    }
}

/// Datasheet `ALS_GAIN` bit pattern for the gain setting.
fn gain_bits(gain: Gain) -> u8 {
    match gain {
        Gain::X1 => 0b00,
        Gain::X2 => 0b01,
        Gain::OneEighth => 0b10,
        Gain::OneFourth => 0b11,
    }
}

/// Datasheet `ALS_IT` bit pattern for the integration time setting.
fn it_bits(it: IntegrationTime) -> u8 {
    match it {
        IntegrationTime::Ms25 => 0b1100,
        IntegrationTime::Ms50 => 0b1000,
        IntegrationTime::Ms100 => 0b0000,
        IntegrationTime::Ms200 => 0b0001,
        IntegrationTime::Ms400 => 0b0010,
        IntegrationTime::Ms800 => 0b0011,
    }
}

/// Lux per count for the given gain × integration time.
///
/// Reference point from the datasheet: gain ×2, IT=800 ms → 0.0036 lx/count.
/// Doubling gain halves the LSB; doubling integration time halves the LSB.
fn lsb_lux(gain: Gain, it: IntegrationTime) -> f32 {
    let gain_ratio = match gain {
        Gain::X1 => 1.0,
        Gain::X2 => 2.0,
        Gain::OneEighth => 0.125,
        Gain::OneFourth => 0.25,
    };
    let it_ms = match it {
        IntegrationTime::Ms25 => 25.0,
        IntegrationTime::Ms50 => 50.0,
        IntegrationTime::Ms100 => 100.0,
        IntegrationTime::Ms200 => 200.0,
        IntegrationTime::Ms400 => 400.0,
        IntegrationTime::Ms800 => 800.0,
    };
    // Reference: gain=×2, IT=800ms → 0.0036 lx/count.
    0.0036 * (2.0 / gain_ratio) * (800.0 / it_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};

    const ADDR: u8 = 0x10;

    fn conf_bytes(gain: Gain, it: IntegrationTime) -> Vec<u8> {
        let conf = (u16::from(gain_bits(gain)) << 11) | (u16::from(it_bits(it)) << 6);
        let le = conf.to_le_bytes();
        vec![REG_ALS_CONF, le[0], le[1]]
    }

    #[test]
    fn init_and_sample_defaults() {
        futures::executor::block_on(async {
            let counts: u16 = 10000;
            let mut mock = Mock::new(&[
                T::write(ADDR, conf_bytes(Gain::X1, IntegrationTime::Ms100)),
                T::write_read(ADDR, vec![REG_ALS], counts.to_le_bytes().to_vec()),
            ]);

            let cfg = Veml7700Config {
                i2c_addr: ADDR,
                ..Veml7700Config::default()
            };
            let mut driver = Veml7700::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();

            // gain=×1, IT=100ms → LSB = 0.0036 * (2/1) * (800/100) = 0.0576
            let expected = 10000.0 * 0.0576;
            assert!((r.lux - expected).abs() < 0.1, "lux={}", r.lux);

            mock.done();
        });
    }

    #[test]
    fn zero_counts_returns_zero_lux() {
        futures::executor::block_on(async {
            let mut mock = Mock::new(&[
                T::write(ADDR, conf_bytes(Gain::X1, IntegrationTime::Ms100)),
                T::write_read(ADDR, vec![REG_ALS], vec![0x00, 0x00]),
            ]);
            let mut driver = Veml7700::new(&Veml7700Config::default());
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();
            assert!(r.lux.abs() < f32::EPSILON, "lux={}", r.lux);
            mock.done();
        });
    }

    #[test]
    fn high_resolution_profile_uses_smallest_lsb() {
        futures::executor::block_on(async {
            // gain=×2, IT=800ms → 0.0036 lx/count.
            let counts: u16 = 10000;
            let mut mock = Mock::new(&[
                T::write(ADDR, conf_bytes(Gain::X2, IntegrationTime::Ms800)),
                T::write_read(ADDR, vec![REG_ALS], counts.to_le_bytes().to_vec()),
            ]);
            let cfg = Veml7700Config {
                i2c_addr: ADDR,
                gain: Gain::X2,
                integration_time: IntegrationTime::Ms800,
            };
            let mut driver = Veml7700::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();
            let expected = 10000.0 * 0.0036;
            assert!((r.lux - expected).abs() < 0.01, "lux={}", r.lux);
            mock.done();
        });
    }

    #[test]
    fn reinit_recovers_after_first_bring_up() {
        // The generated source task re-runs init() to recover a degraded sensor.
        futures::executor::block_on(async {
            let counts: u16 = 10000;
            let mut mock = Mock::new(&[
                T::write(ADDR, conf_bytes(Gain::X1, IntegrationTime::Ms100)),
                T::write(ADDR, conf_bytes(Gain::X1, IntegrationTime::Ms100)), // recovery re-init
                T::write_read(ADDR, vec![REG_ALS], counts.to_le_bytes().to_vec()),
            ]);
            let cfg = Veml7700Config {
                i2c_addr: ADDR,
                ..Veml7700Config::default()
            };
            let mut driver = Veml7700::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            driver.init(&mut mock).await.unwrap(); // recovery re-init
            let r = driver.sample(&mut mock).await.unwrap();
            assert!((r.lux - 10000.0 * 0.0576).abs() < 0.1, "lux={}", r.lux);
            mock.done();
        });
    }
}
