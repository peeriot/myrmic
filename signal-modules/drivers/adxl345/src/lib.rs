//! Analog Devices ADXL345 3-axis accelerometer driver.
//!
//! Communicates over I2C using `embedded-hal-async` and reports
//! X/Y/Z acceleration in g.
//!
//! # Timing
//!
//! The data registers hold a zero image until the first conversion after the
//! device enters measurement mode, so [`Adxl345::init`] sets the output data
//! rate and polls `INT_SOURCE.DATA_READY` (bounded to `DATA_READY_WAIT_PERIODS`
//! output data periods, with `embassy_time` sleeps under `#[cfg(not(test))]`)
//! before returning. [`Adxl345::sample`] does not block.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

// `embassy_time` is only awaited under `#[cfg(not(test))]`; keep it referenced in
// test builds so it never trips `unused_crate_dependencies`.
#[cfg(test)]
use embassy_time as _;

const REG_DEVID: u8 = 0x00;
const REG_BW_RATE: u8 = 0x2C;
const REG_POWER_CTL: u8 = 0x2D;
const REG_INT_SOURCE: u8 = 0x30;
const REG_DATA_FORMAT: u8 = 0x31;
const REG_DATAX0: u8 = 0x32;

const DEVICE_ID: u8 = 0xE5;
const POWER_CTL_MEASURE: u8 = 0x08;
const INT_SOURCE_DATA_READY: u8 = 0x80;

// 100 Hz output data rate, normal power. This is the reset value; writing it
// keeps the first-conversion wait valid when `init` re-runs without a power cycle.
const BW_RATE_100HZ: u8 = 0x0A;
#[cfg(not(test))]
const OUTPUT_DATA_PERIOD_MICROS: u64 = 10_000;

// Output data periods to wait for the first conversion before reporting a
// fault. The datasheet turn-on time is one period plus 1.1 ms; the second
// period covers that remainder.
const DATA_READY_WAIT_PERIODS: u32 = 2;

// FULL_RES = bit 3, Range = 0b11 (±16 g)
const DATA_FORMAT_FULL_RES_16G: u8 = 0x0B;

// Full-resolution mode keeps the sensitivity at 256 LSB/g in every range.
const G_PER_LSB: f32 = 1.0 / 256.0;

/// ADXL345 configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adxl345Config {
    /// I2C address. The common/default address is 0x53.
    pub i2c_addr: u8,
}

impl Default for Adxl345Config {
    fn default() -> Self {
        Self { i2c_addr: 0x53 }
    }
}

/// One XYZ acceleration sample in g.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Adxl345Readings {
    /// X-axis acceleration in g.
    pub accel_x: f32,
    /// Y-axis acceleration in g.
    pub accel_y: f32,
    /// Z-axis acceleration in g.
    pub accel_z: f32,
}

/// Errors returned by the ADXL345 driver.
#[non_exhaustive]
#[derive(Debug)]
pub enum Adxl345Error<E: core::fmt::Debug> {
    /// Underlying I2C error.
    Bus(E),

    /// Device responded but its DEVID register was not 0xE5.
    InvalidDeviceId(u8),

    /// `INT_SOURCE.DATA_READY` stayed clear for `DATA_READY_WAIT_PERIODS`
    /// output data periods after bring-up: the sensor is not converting.
    DataReadyTimeout,
}

impl<E: core::fmt::Debug> From<E> for Adxl345Error<E> {
    fn from(error: E) -> Self {
        Self::Bus(error)
    }
}

/// ADXL345 driver instance.
pub struct Adxl345 {
    addr: u8,
}

impl Adxl345 {
    /// Construct the driver without accessing the I2C bus.
    #[must_use]
    pub fn new(cfg: &Adxl345Config) -> Self {
        Self { addr: cfg.i2c_addr }
    }

    /// Verify the device, configure measurement mode and wait for the first
    /// conversion.
    ///
    /// Safe to call repeatedly: the generated source task re-runs `init` to
    /// recover a sensor that started failing.
    ///
    /// # Errors
    ///
    /// - [`Adxl345Error::Bus`] on any I2C transaction failure.
    /// - [`Adxl345Error::InvalidDeviceId`] if `DEVID` doesn't return `0xE5`.
    /// - [`Adxl345Error::DataReadyTimeout`] if no conversion completes within
    ///   `DATA_READY_WAIT_PERIODS` output data periods.
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), Adxl345Error<I::Error>> {
        let mut devid = [0u8; 1];

        bus.write_read(self.addr, &[REG_DEVID], &mut devid).await?;

        if devid[0] != DEVICE_ID {
            return Err(Adxl345Error::InvalidDeviceId(devid[0]));
        }

        bus.write(self.addr, &[REG_BW_RATE, BW_RATE_100HZ]).await?;

        // Full-resolution ±16 g.
        bus.write(self.addr, &[REG_DATA_FORMAT, DATA_FORMAT_FULL_RES_16G])
            .await?;

        // Measurement mode.
        bus.write(self.addr, &[REG_POWER_CTL, POWER_CTL_MEASURE])
            .await?;

        // The data registers read as zeros until the first conversion lands;
        // poll DATA_READY so the first sample is a real one.
        let mut waited = 0;
        loop {
            let mut int_source = [0u8; 1];
            bus.write_read(self.addr, &[REG_INT_SOURCE], &mut int_source)
                .await?;
            if int_source[0] & INT_SOURCE_DATA_READY != 0 {
                break;
            }
            if waited >= DATA_READY_WAIT_PERIODS {
                return Err(Adxl345Error::DataReadyTimeout);
            }
            waited += 1;
            #[cfg(not(test))]
            embassy_time::Timer::after_micros(OUTPUT_DATA_PERIOD_MICROS).await;
        }

        log::info!("[adxl345] init OK at 0x{:02X}", self.addr);

        Ok(())
    }

    /// Read X, Y and Z acceleration in g.
    ///
    /// # Errors
    ///
    /// [`Adxl345Error::Bus`] on any I2C transaction failure.
    pub async fn sample<I: I2c>(
        &mut self,
        bus: &mut I,
    ) -> Result<Adxl345Readings, Adxl345Error<I::Error>> {
        let mut data = [0u8; 6];

        bus.write_read(self.addr, &[REG_DATAX0], &mut data).await?;

        // ADXL345 axis data is little-endian, two's complement.
        let x_raw = i16::from_le_bytes([data[0], data[1]]);
        let y_raw = i16::from_le_bytes([data[2], data[3]]);
        let z_raw = i16::from_le_bytes([data[4], data[5]]);

        Ok(Adxl345Readings {
            accel_x: f32::from(x_raw) * G_PER_LSB,
            accel_y: f32::from(y_raw) * G_PER_LSB,
            accel_z: f32::from(z_raw) * G_PER_LSB,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};

    const ADDR: u8 = 0x53;
    const RAW_PER_G: i16 = 256;

    /// Probe and configure, up to the point where init starts waiting for the
    /// first conversion.
    fn config_transactions() -> Vec<T> {
        vec![
            T::write_read(ADDR, vec![REG_DEVID], vec![DEVICE_ID]),
            T::write(ADDR, vec![REG_BW_RATE, BW_RATE_100HZ]),
            T::write(ADDR, vec![REG_DATA_FORMAT, DATA_FORMAT_FULL_RES_16G]),
            T::write(ADDR, vec![REG_POWER_CTL, POWER_CTL_MEASURE]),
        ]
    }

    fn int_source_read(int_source: u8) -> T {
        T::write_read(ADDR, vec![REG_INT_SOURCE], vec![int_source])
    }

    fn data_read(x: i16, y: i16, z: i16) -> T {
        let mut raw = Vec::new();
        for axis in [x, y, z] {
            raw.extend_from_slice(&axis.to_le_bytes());
        }
        T::write_read(ADDR, vec![REG_DATAX0], raw)
    }

    #[test]
    fn init_and_sample() {
        futures::executor::block_on(async {
            let mut transactions = config_transactions();
            transactions.push(int_source_read(INT_SOURCE_DATA_READY));
            transactions.push(data_read(RAW_PER_G, -RAW_PER_G, RAW_PER_G / 2));

            let mut mock = Mock::new(&transactions);
            let mut driver = Adxl345::new(&Adxl345Config::default());

            driver.init(&mut mock).await.unwrap();
            let sample = driver.sample(&mut mock).await.unwrap();

            assert!((sample.accel_x - 1.0).abs() < f32::EPSILON);
            assert!((sample.accel_y + 1.0).abs() < f32::EPSILON);
            assert!((sample.accel_z - 0.5).abs() < f32::EPSILON);

            mock.done();
        });
    }

    #[test]
    fn init_waits_for_the_first_conversion() {
        futures::executor::block_on(async {
            let mut transactions = config_transactions();
            for _ in 0..DATA_READY_WAIT_PERIODS {
                transactions.push(int_source_read(0x00));
            }
            transactions.push(int_source_read(INT_SOURCE_DATA_READY));

            let mut mock = Mock::new(&transactions);
            let mut driver = Adxl345::new(&Adxl345Config::default());

            driver.init(&mut mock).await.unwrap();

            mock.done();
        });
    }

    #[test]
    fn init_times_out_when_the_sensor_never_converts() {
        futures::executor::block_on(async {
            let mut transactions = config_transactions();
            for _ in 0..=DATA_READY_WAIT_PERIODS {
                transactions.push(int_source_read(0x00));
            }

            let mut mock = Mock::new(&transactions);
            let mut driver = Adxl345::new(&Adxl345Config::default());

            let result = driver.init(&mut mock).await;

            assert!(matches!(result, Err(Adxl345Error::DataReadyTimeout)));

            mock.done();
        });
    }

    #[test]
    fn rejects_wrong_device_id() {
        futures::executor::block_on(async {
            let wrong_id = !DEVICE_ID;
            let transactions = [T::write_read(ADDR, vec![REG_DEVID], vec![wrong_id])];

            let mut mock = Mock::new(&transactions);
            let mut driver = Adxl345::new(&Adxl345Config::default());

            let result = driver.init(&mut mock).await;

            assert!(matches!(result, Err(Adxl345Error::InvalidDeviceId(id)) if id == wrong_id));

            mock.done();
        });
    }
}
