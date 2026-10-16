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
//! Continuous-mode sampling. The data registers hold a zero image until the
//! first conversion after bring-up completes, so [`WsenItds::init`] polls
//! `STATUS.DRDY` (bounded to two output data periods, derived from the
//! configured ODR, with `embassy_time` sleeps under `#[cfg(not(test))]`)
//! before returning. [`WsenItds::sample`] does not block — it issues a 6-byte
//! burst read of the latest data registers, which block data update keeps
//! consistent across the burst.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

// `embassy_time` is only awaited under `#[cfg(not(test))]`; keep it referenced in
// test builds so it never trips `unused_crate_dependencies`.
#[cfg(test)]
use embassy_time as _;

const REG_DEVICE_ID: u8 = 0x0F;
const REG_CTRL_1: u8 = 0x20;
const REG_CTRL_2: u8 = 0x21;
const REG_CTRL_6: u8 = 0x25;
const REG_STATUS: u8 = 0x27;
const REG_OUT_X_L: u8 = 0x28;

const DEVICE_ID: u8 = 0x44;
const CTRL_2_AUTOINC: u8 = 0x04; // I2C address auto-increment for burst reads
const CTRL_2_BDU: u8 = 0x08; // block data update: output regs frozen until read
const STATUS_DRDY: u8 = 0x01; // new X/Y/Z data available

// Output data periods to wait for the first conversion after bring-up before
// reporting a fault. The datasheet asks for one full period; the second
// covers the ODR switch landing mid-period.
const DRDY_WAIT_PERIODS: u32 = 2;

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

impl Odr {
    /// Output data period in microseconds.
    #[must_use]
    pub const fn period_micros(self) -> u64 {
        match self {
            Odr::Hz1_6 => 625_000,
            Odr::Hz12_5 => 80_000,
            Odr::Hz25 => 40_000,
            Odr::Hz50 => 20_000,
            Odr::Hz100 => 10_000,
            Odr::Hz200 => 5_000,
            Odr::Hz400 => 2_500,
            Odr::Hz800 => 1_250,
            Odr::Hz1600 => 625,
        }
    }
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
    /// `STATUS.DRDY` stayed clear for `DRDY_WAIT_PERIODS` output data
    /// periods after bring-up — the sensor is not converting.
    DataReadyTimeout,
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

    /// (Re-)initialise the sensor: probe (`WHO_AM_I`), enable block data
    /// update and auto-increment burst reads, apply the configured full-scale /
    /// ODR / power mode, then wait for the first conversion.
    ///
    /// Safe to call repeatedly — the generated source task re-runs `init` to
    /// recover a sensor that started failing.
    ///
    /// # Errors
    ///
    /// - [`WsenItdsError::Bus`] on any I2C transaction failure.
    /// - [`WsenItdsError::InvalidId`] if `WHO_AM_I` doesn't return `0x44`.
    /// - [`WsenItdsError::DataReadyTimeout`] if no conversion completes within
    ///   `DRDY_WAIT_PERIODS` output data periods.
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), WsenItdsError<I::Error>> {
        let cfg = self.cfg;
        let (fs_bits, _sensitivity) = fs_config(cfg.full_scale_g);
        let addr = cfg.i2c_addr;

        let mut id = [0u8; 1];
        bus.write_read(addr, &[REG_DEVICE_ID], &mut id).await?;
        if id[0] != DEVICE_ID {
            return Err(WsenItdsError::InvalidId(id[0]));
        }

        // Block data update keeps a burst from mixing two samples; auto-increment
        // is required for the 6-byte burst read.
        bus.write(addr, &[REG_CTRL_2, CTRL_2_AUTOINC | CTRL_2_BDU])
            .await?;
        // Set full-scale
        bus.write(addr, &[REG_CTRL_6, fs_bits]).await?;
        // CTRL_1: bits 7:4 = ODR, bits 3:2 = power_mode.
        let ctrl_1 = ((cfg.odr as u8) << 4) | ((cfg.power_mode as u8) << 2);
        bus.write(addr, &[REG_CTRL_1, ctrl_1]).await?;

        // The data registers read as zeros until the first conversion at the
        // new ODR lands; poll DRDY so the first sample is a real one.
        let mut waited = 0;
        loop {
            let mut status = [0u8; 1];
            bus.write_read(addr, &[REG_STATUS], &mut status).await?;
            if status[0] & STATUS_DRDY != 0 {
                break;
            }
            if waited >= DRDY_WAIT_PERIODS {
                return Err(WsenItdsError::DataReadyTimeout);
            }
            waited += 1;
            #[cfg(not(test))]
            embassy_time::Timer::after_micros(cfg.odr.period_micros()).await;
        }

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

    /// Probe and configure, up to the point where init starts waiting for the
    /// first conversion.
    fn config_transactions(fs_bits: u8, ctrl_1_val: u8) -> Vec<T> {
        vec![
            T::write_read(ADDR, vec![REG_DEVICE_ID], vec![DEVICE_ID]),
            T::write(ADDR, vec![REG_CTRL_2, 0x0C]), // IF_ADD_INC | BDU
            T::write(ADDR, vec![REG_CTRL_6, fs_bits]),
            T::write(ADDR, vec![REG_CTRL_1, ctrl_1_val]),
        ]
    }

    fn status_read(status: u8) -> T {
        T::write_read(ADDR, vec![REG_STATUS], vec![status])
    }

    /// A full bring-up whose first `STATUS` poll already reports data ready.
    fn init_transactions(fs_bits: u8, ctrl_1_val: u8) -> Vec<T> {
        let mut txns = config_transactions(fs_bits, ctrl_1_val);
        txns.push(status_read(STATUS_DRDY));
        txns
    }

    /// Burst read returning ~1 g on Z at ±2 g.
    #[allow(clippy::cast_possible_truncation)]
    fn one_g_on_z_read() -> T {
        let (_fs_bits, sens) = fs_config(FullScale::G2);
        let counts_z = (1.0_f32 / sens) as i16;
        let raw_z = counts_z.to_le_bytes();
        T::write_read(
            ADDR,
            vec![REG_OUT_X_L],
            vec![0x00, 0x00, 0x00, 0x00, raw_z[0], raw_z[1]],
        )
    }

    #[test]
    fn first_sample_waits_for_data_ready() {
        // The data registers hold a zero image until the first conversion
        // completes: init must keep polling STATUS until DRDY is set, so the
        // first published value comes from the read after the ready poll.
        futures::executor::block_on(async {
            let (fs_bits, _sens) = fs_config(FullScale::G2);
            let ctrl_1_val = ((Odr::Hz100 as u8) << 4) | ((PowerMode::Normal as u8) << 2);
            let mut txns = config_transactions(fs_bits, ctrl_1_val);
            txns.push(status_read(0x00));
            txns.push(status_read(STATUS_DRDY));
            txns.push(one_g_on_z_read());
            let mut mock = Mock::new(&txns);

            let cfg = WsenItdsConfig {
                i2c_addr: ADDR,
                ..WsenItdsConfig::default()
            };
            let mut driver = WsenItds::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();
            assert!((r.accel_z - 1.0).abs() < 0.01, "z={}", r.accel_z);

            mock.done();
        });
    }

    #[test]
    fn init_fails_when_no_conversion_arrives() {
        // A sensor that never asserts DRDY within the wait budget is a
        // bring-up fault, reported from init rather than published as zeros.
        futures::executor::block_on(async {
            let (fs_bits, _sens) = fs_config(FullScale::G2);
            let ctrl_1_val = ((Odr::Hz100 as u8) << 4) | ((PowerMode::Normal as u8) << 2);
            let mut txns = config_transactions(fs_bits, ctrl_1_val);
            for _ in 0..=DRDY_WAIT_PERIODS {
                txns.push(status_read(0x00));
            }
            let mut mock = Mock::new(&txns);

            let cfg = WsenItdsConfig {
                i2c_addr: ADDR,
                ..WsenItdsConfig::default()
            };
            let mut driver = WsenItds::new(&cfg);
            let result = driver.init(&mut mock).await;
            assert!(matches!(result, Err(WsenItdsError::DataReadyTimeout)));

            mock.done();
        });
    }

    #[test]
    fn odr_period_matches_rate() {
        assert_eq!(Odr::Hz1_6.period_micros(), 625_000);
        assert_eq!(Odr::Hz100.period_micros(), 10_000);
        assert_eq!(Odr::Hz1600.period_micros(), 625);
    }

    #[test]
    fn init_and_sample_2g_defaults() {
        futures::executor::block_on(async {
            let (fs_bits, _sens) = fs_config(FullScale::G2);
            let ctrl_1_val = ((Odr::Hz100 as u8) << 4) | ((PowerMode::Normal as u8) << 2);
            let mut txns = init_transactions(fs_bits, ctrl_1_val);
            txns.push(one_g_on_z_read());
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
    fn reinit_recovers_after_first_bring_up() {
        // The generated source task re-runs init() to recover a degraded sensor.
        futures::executor::block_on(async {
            let (fs_bits, _sens) = fs_config(FullScale::G2);
            let ctrl_1_val = ((Odr::Hz100 as u8) << 4) | ((PowerMode::Normal as u8) << 2);

            let mut txns = init_transactions(fs_bits, ctrl_1_val);
            txns.extend(init_transactions(fs_bits, ctrl_1_val)); // recovery re-init
            txns.push(one_g_on_z_read());
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
