//! Würth Elektronik WSEN-ITDS 3-axis MEMS accelerometer driver.
//!
//! 3-axis ±2/4/8/16 g MEMS accelerometer over I2C. Returns acceleration in
//! g (gravity) for each axis, derived from the raw 14-bit signed counts and
//! the sensitivity for the selected full-scale range.
//!
//! # Configuration
//!
//! [`WsenItdsConfig`] exposes the datasheet knobs that affect the readings:
//!
//! - **Full-scale range** (`full_scale_g`) — see [`FullScale`].
//!   ±2/4/8/16 g. Lower range → higher resolution per count, but limited
//!   measurable acceleration.
//! - **Output data rate** (`odr`) — see [`Odr`]. Sets how often the sensor
//!   produces a new sample (1.6 Hz to 1600 Hz).
//! - **Power mode** (`power_mode`) — see [`PowerMode`]. Trades
//!   noise/resolution for current consumption.
//!
//! Defaults: ±2 g, 100 Hz ODR, normal-mode resolution.
//!
//! # Timing
//!
//! Continuous-mode sampling. [`WsenItds::sample`] does not block — it issues
//! a 6-byte burst read of the latest data registers. After init the
//! datasheet recommends waiting one full ODR period before the first read.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

const REG_DEVICE_ID: u8 = 0x0F;
const REG_CTRL_1: u8 = 0x20;
const REG_CTRL_2: u8 = 0x21;
const REG_CTRL_6: u8 = 0x25;
const REG_OUT_X_L: u8 = 0x28;

const DEVICE_ID: u8 = 0x44;
const CTRL_2_AUTOINC: u8 = 0x04; // I2C address auto-increment for burst reads

/// Full-scale measurement range in g.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullScale {
    /// ±2 g — highest resolution.
    G2 = 2,
    /// ±4 g.
    G4 = 4,
    /// ±8 g.
    G8 = 8,
    /// ±16 g — widest range.
    G16 = 16,
}

/// Output data rate — how often the sensor produces a new sample.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Odr {
    /// Power-down (no sampling).
    PowerDown = 0,
    /// 1.6 Hz (low-power only).
    Hz1_6 = 1,
    /// 12.5 Hz.
    Hz12_5 = 2,
    /// 25 Hz.
    Hz25 = 3,
    /// 50 Hz.
    Hz50 = 4,
    /// 100 Hz (default).
    Hz100 = 5,
    /// 200 Hz.
    Hz200 = 6,
    /// 400 Hz.
    Hz400 = 7,
    /// 800 Hz.
    Hz800 = 8,
    /// 1600 Hz (high-performance only).
    Hz1600 = 9,
}

/// Power / resolution mode.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerMode {
    /// Low-power 12-bit mode (lowest current).
    Low = 0,
    /// Normal-mode 14-bit (default).
    Normal = 1,
    /// High-performance 14-bit (highest current, supports 1600 Hz ODR).
    High = 2,
}

/// Driver configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsenItdsConfig {
    /// I2C address. Factory default `0x18` (SA0 to GND); `0x19` when SA0 is
    /// tied to VDD.
    pub i2c_addr: u8,
    /// Full-scale measurement range.
    pub full_scale_g: FullScale,
    /// Output data rate.
    pub odr: Odr,
    /// Power / resolution mode.
    pub power_mode: PowerMode,
}

impl Default for WsenItdsConfig {
    /// Address `0x18`, ±2 g, 100 Hz ODR, normal-mode resolution.
    fn default() -> Self {
        Self {
            i2c_addr: 0x18,
            full_scale_g: FullScale::G2,
            odr: Odr::Hz100,
            power_mode: PowerMode::Normal,
        }
    }
}

/// One 3-axis acceleration reading.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WsenItdsReadings {
    /// X-axis acceleration in g.
    pub accel_x: f32,
    /// Y-axis acceleration in g.
    pub accel_y: f32,
    /// Z-axis acceleration in g.
    pub accel_z: f32,
}

/// Errors returned by the WSEN-ITDS driver.
#[non_exhaustive]
#[derive(Debug)]
pub enum WsenItdsError<E: core::fmt::Debug> {
    /// Underlying I2C bus error.
    Bus(E),
    /// `WHO_AM_I` register did not return `0x44`.
    InvalidId(u8),
}

impl<E: core::fmt::Debug> From<E> for WsenItdsError<E> {
    fn from(e: E) -> Self {
        Self::Bus(e)
    }
}

/// WSEN-ITDS driver instance.
pub struct WsenItds {
    cfg: WsenItdsConfig,
    sensitivity: f32,
}

impl WsenItds {
    /// Construct a driver instance without touching the bus.
    ///
    /// The per-count sensitivity for the configured full-scale is computed here;
    /// the sensor itself is **not** brought up until [`WsenItds::init`] is
    /// called, which must happen before [`WsenItds::sample`].
    #[must_use]
    pub fn new(cfg: &WsenItdsConfig) -> Self {
        let (_fs_bits, sensitivity) = fs_config(cfg.full_scale_g);
        Self {
            cfg: *cfg,
            sensitivity,
        }
    }

    /// (Re-)initialise the sensor: probe (`WHO_AM_I`), apply the configured
    /// full-scale / ODR / power mode, and enable auto-increment burst reads.
    ///
    /// Safe to call repeatedly — the generated source task re-runs `init` to
    /// recover a sensor that started failing.
    ///
    /// # Errors
    ///
    /// - [`WsenItdsError::Bus`] on any I2C transaction failure.
    /// - [`WsenItdsError::InvalidId`] if `WHO_AM_I` doesn't return `0x44`.
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), WsenItdsError<I::Error>> {
        let cfg = self.cfg;
        let (fs_bits, _sensitivity) = fs_config(cfg.full_scale_g);
        let addr = cfg.i2c_addr;

        let mut id = [0u8; 1];
        bus.write_read(addr, &[REG_DEVICE_ID], &mut id).await?;
        if id[0] != DEVICE_ID {
            return Err(WsenItdsError::InvalidId(id[0]));
        }

        // Enable auto-increment (required for burst read of 6 bytes)
        bus.write(addr, &[REG_CTRL_2, CTRL_2_AUTOINC]).await?;
        // Set full-scale
        bus.write(addr, &[REG_CTRL_6, fs_bits]).await?;
        // CTRL_1: bits 7:4 = ODR, bits 3:2 = power_mode.
        let ctrl_1 = ((cfg.odr as u8) << 4) | ((cfg.power_mode as u8) << 2);
        bus.write(addr, &[REG_CTRL_1, ctrl_1]).await?;

        log::info!(
            "[wsen-itds] init OK at 0x{addr:02X}, FS={:?}, ODR={:?}",
            cfg.full_scale_g,
            cfg.odr
        );
        Ok(())
    }

    /// Read X/Y/Z acceleration in one burst.
    ///
    /// # Errors
    ///
    /// [`WsenItdsError::Bus`] on any I2C transaction failure.
    pub async fn sample<I: I2c>(
        &mut self,
        bus: &mut I,
    ) -> Result<WsenItdsReadings, WsenItdsError<I::Error>> {
        // Burst-read 6 bytes: X_L, X_H, Y_L, Y_H, Z_L, Z_H
        let mut raw = [0u8; 6];
        bus.write_read(self.cfg.i2c_addr, &[REG_OUT_X_L], &mut raw)
            .await?;

        let ax = f32::from(i16::from_le_bytes([raw[0], raw[1]])) * self.sensitivity;
        let ay = f32::from(i16::from_le_bytes([raw[2], raw[3]])) * self.sensitivity;
        let az = f32::from(i16::from_le_bytes([raw[4], raw[5]])) * self.sensitivity;

        log::debug!("[wsen-itds] X={ax:.3}g Y={ay:.3}g Z={az:.3}g");
        Ok(WsenItdsReadings {
            accel_x: ax,
            accel_y: ay,
            accel_z: az,
        })
    }
}

/// Returns (`CTRL_6` FS bits, sensitivity in g/count) for a full-scale range.
/// Sensitivity values from Würth WE eMagin SDK (µg/digit table, 14-bit normal mode).
fn fs_config(full_scale: FullScale) -> (u8, f32) {
    match full_scale {
        FullScale::G2 => (0x00, 0.000_061),
        FullScale::G4 => (0x10, 0.000_122),
        FullScale::G8 => (0x20, 0.000_244),
        FullScale::G16 => (0x30, 0.000_488),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};

    const ADDR: u8 = 0x19;

    fn init_transactions(fs_bits: u8, ctrl_1_val: u8) -> Vec<T> {
        vec![
            T::write_read(ADDR, vec![REG_DEVICE_ID], vec![DEVICE_ID]),
            T::write(ADDR, vec![REG_CTRL_2, CTRL_2_AUTOINC]),
            T::write(ADDR, vec![REG_CTRL_6, fs_bits]),
            T::write(ADDR, vec![REG_CTRL_1, ctrl_1_val]),
        ]
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn init_and_sample_2g_defaults() {
        futures::executor::block_on(async {
            let (fs_bits, sens) = fs_config(FullScale::G2);
            let counts_z = (1.0_f32 / sens) as i16;
            let raw_z = counts_z.to_le_bytes();

            let ctrl_1_val = ((Odr::Hz100 as u8) << 4) | ((PowerMode::Normal as u8) << 2);
            let mut txns = init_transactions(fs_bits, ctrl_1_val);
            txns.push(T::write_read(
                ADDR,
                vec![REG_OUT_X_L],
                vec![0x00, 0x00, 0x00, 0x00, raw_z[0], raw_z[1]],
            ));
            let mut mock = Mock::new(&txns);

            let cfg = WsenItdsConfig {
                i2c_addr: ADDR,
                ..WsenItdsConfig::default()
            };
            let mut driver = WsenItds::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();

            assert!(r.accel_x.abs() < 0.01, "x={}", r.accel_x);
            assert!(r.accel_y.abs() < 0.01, "y={}", r.accel_y);
            assert!((r.accel_z - 1.0).abs() < 0.01, "z={}", r.accel_z);

            mock.done();
        });
    }

    #[test]
    fn wrong_device_id_returns_error() {
        futures::executor::block_on(async {
            let mut mock = Mock::new(&[T::write_read(ADDR, vec![REG_DEVICE_ID], vec![0x00])]);
            let cfg = WsenItdsConfig {
                i2c_addr: ADDR,
                ..WsenItdsConfig::default()
            };
            let mut driver = WsenItds::new(&cfg);
            let result = driver.init(&mut mock).await;
            assert!(matches!(result, Err(WsenItdsError::InvalidId(0x00))));
            mock.done();
        });
    }

    #[test]
    fn custom_odr_and_full_scale_written_to_registers() {
        futures::executor::block_on(async {
            let (fs_bits, _sens) = fs_config(FullScale::G8);
            let ctrl_1_val = ((Odr::Hz25 as u8) << 4) | ((PowerMode::Low as u8) << 2);
            let txns = init_transactions(fs_bits, ctrl_1_val);
            let mut mock = Mock::new(&txns);
            let cfg = WsenItdsConfig {
                i2c_addr: ADDR,
                full_scale_g: FullScale::G8,
                odr: Odr::Hz25,
                power_mode: PowerMode::Low,
            };
            let mut driver = WsenItds::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            mock.done();
        });
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn reinit_recovers_after_first_bring_up() {
        // The generated source task re-runs init() to recover a degraded sensor.
        futures::executor::block_on(async {
            let (fs_bits, sens) = fs_config(FullScale::G2);
            let counts_z = (1.0_f32 / sens) as i16;
            let raw_z = counts_z.to_le_bytes();
            let ctrl_1_val = ((Odr::Hz100 as u8) << 4) | ((PowerMode::Normal as u8) << 2);

            let mut txns = init_transactions(fs_bits, ctrl_1_val);
            txns.extend(init_transactions(fs_bits, ctrl_1_val)); // recovery re-init
            txns.push(T::write_read(
                ADDR,
                vec![REG_OUT_X_L],
                vec![0x00, 0x00, 0x00, 0x00, raw_z[0], raw_z[1]],
            ));
            let mut mock = Mock::new(&txns);

            let cfg = WsenItdsConfig {
                i2c_addr: ADDR,
                ..WsenItdsConfig::default()
            };
            let mut driver = WsenItds::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            driver.init(&mut mock).await.unwrap(); // recovery re-init
            let r = driver.sample(&mut mock).await.unwrap();
            assert!((r.accel_z - 1.0).abs() < 0.01, "z={}", r.accel_z);
            mock.done();
        });
    }
}
