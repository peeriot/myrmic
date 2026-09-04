//! Bosch BMP180 barometric pressure and temperature sensor driver.
//!
//! Driver for the legacy Bosch BMP180 — pressure and temperature over I2C
//! at a fixed address (`0x77`). Readings are in °C and hPa, derived from the
//! on-chip calibration table using the datasheet §4.1.2 fixed-point
//! compensation algorithm.
//!
//! # Configuration
//!
//! [`Bmp180Config`] exposes the single datasheet knob, **pressure
//! oversampling** (`oss`) — see [`Oversampling`]. Higher oversampling reduces
//! noise at the cost of a longer measurement window (4.5 / 7.5 / 13.5 / 25.5 ms
//! for OSS = 0/1/2/3). Defaults to ultra-low power (`Single`).
//!
//! # Timing
//!
//! The BMP180 is **command-driven**: each [`Bmp180::sample`] call issues a
//! temperature measurement command, reads the result, issues a pressure
//! command, and reads the result. The driver does **not** insert delays
//! between the command and the read — the datasheet requires:
//!
//! - Temperature: 4.5 ms after `0x2E` is written.
//! - Pressure: 4.5 / 7.5 / 13.5 / 25.5 ms depending on `oss`.
//!
//! These delays are the **caller's responsibility**. In the Signal Layer the
//! codegen-emitted source task owns the sample interval and is expected to
//! be slow enough (≥100 ms typical) that conversion is already complete.
//! Stand-alone consumers should add a [`embedded_hal_async::delay::DelayNs`]
//! wait between commands.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

const CHIP_ID: u8 = 0x55;
const REG_CHIP_ID: u8 = 0xD0;
const REG_CALIB: u8 = 0xAA; // 22 bytes: AC1-AC6, B1-B2, MB-MC-MD
const REG_CTRL: u8 = 0xF4;
const REG_DATA: u8 = 0xF6;
const CMD_TEMP: u8 = 0x2E;
const CMD_PRESS_BASE: u8 = 0x34; // OR'd with (oss << 6)

/// Pressure oversampling — trades noise reduction for a longer conversion window.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Oversampling {
    /// Single sample (4.5 ms conversion, ultra-low power).
    Single = 0,
    /// 2 samples averaged (7.5 ms conversion).
    X2 = 1,
    /// 4 samples averaged (13.5 ms conversion).
    X4 = 2,
    /// 8 samples averaged (25.5 ms conversion, ultra-high resolution).
    X8 = 3,
}

/// Driver configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bmp180Config {
    /// I2C address. The BMP180 has a fixed address (`0x77`); the field exists
    /// for symmetry with other I2C drivers in the Signal Layer.
    pub i2c_addr: u8,
    /// Pressure oversampling. Higher values lower noise but lengthen the
    /// conversion window (4.5 → 25.5 ms).
    pub oss: Oversampling,
}

impl Default for Bmp180Config {
    /// Address `0x77`, ultra-low-power pressure oversampling.
    fn default() -> Self {
        Self {
            i2c_addr: 0x77,
            oss: Oversampling::Single,
        }
    }
}

/// One full set of compensated sensor readings.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bmp180Readings {
    /// Temperature in degrees Celsius (resolution 0.1°C).
    pub temperature: f32,
    /// Atmospheric pressure in hPa (resolution depends on `oss`).
    pub pressure: f32,
}

/// Errors returned by the BMP180 driver.
#[non_exhaustive]
#[derive(Debug)]
pub enum Bmp180Error<E: core::fmt::Debug> {
    /// Underlying I2C bus error.
    Bus(E),
    /// Chip ID register did not return `0x55` — likely wrong device or wiring.
    InvalidId(u8),
}

impl<E: core::fmt::Debug> From<E> for Bmp180Error<E> {
    fn from(e: E) -> Self {
        Self::Bus(e)
    }
}

#[derive(Default)]
struct Calibration {
    ac1: i16,
    ac2: i16,
    ac3: i16,
    ac4: u16,
    ac5: u16,
    ac6: u16,
    b1: i16,
    b2: i16,
    mc: i16,
    md: i16,
}

/// BMP180 driver instance.
///
/// Construct with [`Bmp180::new`], bring the sensor up with [`Bmp180::init`],
/// then read with [`Bmp180::sample`].
pub struct Bmp180 {
    cfg: Bmp180Config,
    cal: Calibration,
}

impl Bmp180 {
    /// Construct a driver instance without touching the bus.
    ///
    /// The sensor is **not** initialised yet — call [`Bmp180::init`] before
    /// [`Bmp180::sample`]. Calibration is zeroed until `init` loads it.
    #[must_use]
    pub fn new(cfg: &Bmp180Config) -> Self {
        Self {
            cfg: *cfg,
            cal: Calibration::default(),
        }
    }

    /// (Re-)initialise the sensor: verify the chip ID and load its calibration.
    ///
    /// Safe to call repeatedly — the generated source task re-runs `init` to
    /// recover a sensor that started failing. The stored calibration is only
    /// replaced once a full read succeeds.
    ///
    /// # Errors
    ///
    /// - [`Bmp180Error::Bus`] on any I2C transaction failure.
    /// - [`Bmp180Error::InvalidId`] if the chip ID register does not read `0x55`.
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), Bmp180Error<I::Error>> {
        let addr = self.cfg.i2c_addr;

        let mut id = [0u8; 1];
        bus.write_read(addr, &[REG_CHIP_ID], &mut id).await?;
        if id[0] != CHIP_ID {
            return Err(Bmp180Error::InvalidId(id[0]));
        }

        // Read 22 calibration bytes
        let mut raw = [0u8; 22];
        bus.write_read(addr, &[REG_CALIB], &mut raw).await?;

        self.cal = Calibration {
            ac1: i16::from_be_bytes([raw[0], raw[1]]),
            ac2: i16::from_be_bytes([raw[2], raw[3]]),
            ac3: i16::from_be_bytes([raw[4], raw[5]]),
            ac4: u16::from_be_bytes([raw[6], raw[7]]),
            ac5: u16::from_be_bytes([raw[8], raw[9]]),
            ac6: u16::from_be_bytes([raw[10], raw[11]]),
            b1: i16::from_be_bytes([raw[12], raw[13]]),
            b2: i16::from_be_bytes([raw[14], raw[15]]),
            // raw[16..18] = MB (unused)
            mc: i16::from_be_bytes([raw[18], raw[19]]),
            md: i16::from_be_bytes([raw[20], raw[21]]),
        };

        log::info!("[bmp180] init OK at 0x{addr:02X}");
        Ok(())
    }

    /// Issue a temperature command, read the raw result, issue a pressure
    /// command, read the raw result, then compensate both.
    ///
    /// **The caller is responsible for the inter-command delays** mandated
    /// by the datasheet (4.5 ms for temperature; 4.5 / 7.5 / 13.5 / 25.5 ms
    /// for pressure depending on `oss`). When invoked from the Signal Layer
    /// codegen-emitted task the `sample_interval_ms` is the only delay; for
    /// stand-alone use, wait between the temp and pressure phases yourself.
    ///
    /// # Errors
    ///
    /// [`Bmp180Error::Bus`] on any I2C transaction failure.
    pub async fn sample<I: I2c>(
        &mut self,
        bus: &mut I,
    ) -> Result<Bmp180Readings, Bmp180Error<I::Error>> {
        let addr = self.cfg.i2c_addr;
        let oss = self.cfg.oss as u8;

        // Read raw temperature
        bus.write(addr, &[REG_CTRL, CMD_TEMP]).await?;
        // In real hardware: delay 4.5 ms here. Caller owns the timing.
        let mut raw = [0u8; 2];
        bus.write_read(addr, &[REG_DATA], &mut raw).await?;
        let ut = i32::from(i16::from_be_bytes([raw[0], raw[1]]));

        // Read raw pressure
        let press_cmd = CMD_PRESS_BASE | (oss << 6);
        bus.write(addr, &[REG_CTRL, press_cmd]).await?;
        // In real hardware: delay 4.5/7.5/13.5/25.5 ms depending on oss.
        let mut raw3 = [0u8; 3];
        bus.write_read(addr, &[REG_DATA], &mut raw3).await?;
        // Datasheet §3.5: pressure result is a 19-bit value, right-shifted by (8 - oss).
        let raw_p =
            (i32::from(raw3[0]) << 16 | i32::from(raw3[1]) << 8 | i32::from(raw3[2])) >> (8 - oss);

        let (b5, temperature) = compensate_temperature(ut, &self.cal);
        let pressure = compensate_pressure(raw_p, b5, oss, &self.cal);

        log::debug!("[bmp180] T={temperature:.1}°C P={pressure:.1}hPa");
        Ok(Bmp180Readings {
            temperature,
            pressure,
        })
    }
}

// Compensation (BMP180 datasheet §4.1.2 formulas)

// BMP180 datasheet §4.1.2 formula: final result is temperature in 0.1°C units divided by 10;
// the i32→f32 precision loss is acceptable for the sensor's ±0.1°C output resolution.
#[allow(clippy::cast_precision_loss)]
fn compensate_temperature(ut: i32, cal: &Calibration) -> (i32, f32) {
    let x1 = ((ut - i32::from(cal.ac6)) * i32::from(cal.ac5)) >> 15;
    let x2 = (i32::from(cal.mc) * 2048) / (x1 + i32::from(cal.md));
    let b5 = x1 + x2;
    (b5, ((b5 + 8) >> 4) as f32 / 10.0)
}

// BMP180 datasheet §4.1.2 pressure compensation algorithm. The mixed-sign integer
// arithmetic and final i32→f32 cast are specified by the datasheet; deviating would
// produce wrong results. Truncation and wrap-around are part of the fixed-point math.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
fn compensate_pressure(up: i32, b5: i32, oss: u8, cal: &Calibration) -> f32 {
    let b6 = b5 - 4000;
    let x1 = (i32::from(cal.b2) * ((b6 * b6) >> 12)) >> 11;
    let x2 = (i32::from(cal.ac2) * b6) >> 11;
    let x3 = x1 + x2;
    let b3 = (((i32::from(cal.ac1) * 4 + x3) << oss) + 2) >> 2;
    let x1 = (i32::from(cal.ac3) * b6) >> 13;
    let x2 = (i32::from(cal.b1) * ((b6 * b6) >> 12)) >> 16;
    let x3 = (x1 + x2 + 2) >> 2;
    let b4 = (u32::from(cal.ac4) * (x3 + 32768).cast_unsigned()) >> 15;
    let b7 = up.cast_unsigned().wrapping_sub(b3.cast_unsigned()) * (50000 >> oss);
    let p = if b7 < 0x8000_0000 {
        (b7 * 2) / b4
    } else {
        (b7 / b4) * 2
    } as i32;
    let x1 = (p >> 8) * (p >> 8);
    let x1 = (x1 * 3038) >> 16;
    let x2 = (-7357 * p) >> 16;
    (p + ((x1 + x2 + 3791) >> 4)) as f32 / 100.0 // Pa → hPa
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};

    const ADDR: u8 = 0x77;

    fn cal_bytes() -> Vec<u8> {
        // Datasheet example calibration values
        let vals: [(u8, u8); 11] = [
            (0x1D, 0xFD), // AC1
            (0xFF, 0xB8), // AC2 = -72
            (0xC7, 0xD1), // AC3 = -14383
            (0x7F, 0xE5), // AC4 = 32741
            (0x7F, 0xE5), // AC5 = 32741
            (0x60, 0x00), // AC6 = 24576
            (0x17, 0xD0), // B1  = 6096
            (0x00, 0x04), // B2  = 4
            (0x80, 0x00), // MB  = -32768 (unused)
            (0xD4, 0xBD), // MC  = -11075
            (0x0A, 0x7C), // MD  = 2684
        ];
        vals.iter().flat_map(|(h, l)| [*h, *l]).collect()
    }

    #[test]
    fn init_and_sample() {
        futures::executor::block_on(async {
            let mut mock = Mock::new(&[
                T::write_read(ADDR, vec![REG_CHIP_ID], vec![CHIP_ID]),
                T::write_read(ADDR, vec![REG_CALIB], cal_bytes()),
                // sample: temp measurement
                T::write(ADDR, vec![REG_CTRL, CMD_TEMP]),
                T::write_read(ADDR, vec![REG_DATA], vec![0x6C, 0xEB]), // UT = 27883
                // sample: pressure measurement (oss=0 → cmd 0x34)
                T::write(ADDR, vec![REG_CTRL, CMD_PRESS_BASE]),
                T::write_read(ADDR, vec![REG_DATA], vec![0x5D, 0x29, 0x00]),
            ]);

            let cfg = Bmp180Config {
                i2c_addr: ADDR,
                ..Bmp180Config::default()
            };
            let mut driver = Bmp180::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let readings = driver.sample(&mut mock).await.unwrap();

            assert!(
                readings.temperature > -40.0 && readings.temperature < 85.0,
                "T={}",
                readings.temperature
            );
            assert!(
                readings.pressure > 300.0 && readings.pressure < 1100.0,
                "P={}",
                readings.pressure
            );

            mock.done();
        });
    }

    #[test]
    fn wrong_chip_id_returns_error() {
        futures::executor::block_on(async {
            let mut mock = Mock::new(&[T::write_read(ADDR, vec![REG_CHIP_ID], vec![0xAA])]);
            let mut driver = Bmp180::new(&Bmp180Config::default());
            let result = driver.init(&mut mock).await;
            assert!(matches!(result, Err(Bmp180Error::InvalidId(0xAA))));
            mock.done();
        });
    }

    #[test]
    fn higher_oss_uses_different_pressure_command() {
        futures::executor::block_on(async {
            // oss=X4 (2) → press_cmd = 0x34 | (2<<6) = 0xB4
            let press_cmd = CMD_PRESS_BASE | ((Oversampling::X4 as u8) << 6);
            let mut mock = Mock::new(&[
                T::write_read(ADDR, vec![REG_CHIP_ID], vec![CHIP_ID]),
                T::write_read(ADDR, vec![REG_CALIB], cal_bytes()),
                T::write(ADDR, vec![REG_CTRL, CMD_TEMP]),
                T::write_read(ADDR, vec![REG_DATA], vec![0x6C, 0xEB]),
                T::write(ADDR, vec![REG_CTRL, press_cmd]),
                T::write_read(ADDR, vec![REG_DATA], vec![0x5D, 0x29, 0x00]),
            ]);
            let cfg = Bmp180Config {
                i2c_addr: ADDR,
                oss: Oversampling::X4,
            };
            let mut driver = Bmp180::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let _ = driver.sample(&mut mock).await.unwrap();
            mock.done();
        });
    }

    #[test]
    fn reinit_recovers_after_first_bring_up() {
        // The generated source task re-runs init() to recover a degraded sensor.
        futures::executor::block_on(async {
            let mut mock = Mock::new(&[
                // first bring-up
                T::write_read(ADDR, vec![REG_CHIP_ID], vec![CHIP_ID]),
                T::write_read(ADDR, vec![REG_CALIB], cal_bytes()),
                // recovery re-init
                T::write_read(ADDR, vec![REG_CHIP_ID], vec![CHIP_ID]),
                T::write_read(ADDR, vec![REG_CALIB], cal_bytes()),
                // sample
                T::write(ADDR, vec![REG_CTRL, CMD_TEMP]),
                T::write_read(ADDR, vec![REG_DATA], vec![0x6C, 0xEB]),
                T::write(ADDR, vec![REG_CTRL, CMD_PRESS_BASE]),
                T::write_read(ADDR, vec![REG_DATA], vec![0x5D, 0x29, 0x00]),
            ]);
            let cfg = Bmp180Config {
                i2c_addr: ADDR,
                ..Bmp180Config::default()
            };
            let mut driver = Bmp180::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            driver.init(&mut mock).await.unwrap(); // recovery re-init
            let readings = driver.sample(&mut mock).await.unwrap();
            assert!(readings.temperature > -40.0 && readings.temperature < 85.0);
            mock.done();
        });
    }
}
