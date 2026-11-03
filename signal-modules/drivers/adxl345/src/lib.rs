//! Analog Devices ADXL345 3-axis accelerometer driver.
//!
//! Communicates over I2C using `embedded-hal-async` and reports
//! X/Y/Z acceleration in g.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

const REG_DEVID: u8 = 0x00;
const REG_POWER_CTL: u8 = 0x2D;
const REG_DATA_FORMAT: u8 = 0x31;
const REG_DATAX0: u8 = 0x32;

const DEVICE_ID: u8 = 0xE5;
const POWER_CTL_MEASURE: u8 = 0x08;

// FULL_RES = bit 3, Range = 0b11 (±16 g)
const DATA_FORMAT_FULL_RES_16G: u8 = 0x0B;

// In full-resolution mode the ADXL345 scale factor is approximately
// 3.9 mg/LSB.
const G_PER_LSB: f32 = 0.0039;

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
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// Errors returned by the ADXL345 driver.
#[non_exhaustive]
#[derive(Debug)]
pub enum Adxl345Error<E: core::fmt::Debug> {
    /// Underlying I2C error.
    Bus(E),

    /// Device responded but its DEVID register was not 0xE5.
    InvalidDeviceId(u8),
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

    /// Verify the device and configure measurement mode.
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), Adxl345Error<I::Error>> {
        let mut devid = [0u8; 1];

        bus.write_read(self.addr, &[REG_DEVID], &mut devid).await?;

        if devid[0] != DEVICE_ID {
            return Err(Adxl345Error::InvalidDeviceId(devid[0]));
        }

        // Full-resolution ±16 g.
        bus.write(self.addr, &[REG_DATA_FORMAT, DATA_FORMAT_FULL_RES_16G])
            .await?;

        // Measurement mode.
        bus.write(self.addr, &[REG_POWER_CTL, POWER_CTL_MEASURE])
            .await?;

        log::info!("[adxl345] init OK at 0x{:02X}", self.addr);

        Ok(())
    }

    /// Read X, Y and Z acceleration in g.
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
            x: f32::from(x_raw) * G_PER_LSB,
            y: f32::from(y_raw) * G_PER_LSB,
            z: f32::from(z_raw) * G_PER_LSB,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};

    const ADDR: u8 = 0x53;

    #[test]
    fn init_and_sample() {
        futures::executor::block_on(async {
            let transactions = [
                // Read DEVID.
                T::write_read(ADDR, vec![REG_DEVID], vec![DEVICE_ID]),
                // Configure full-resolution ±16 g.
                T::write(ADDR, vec![REG_DATA_FORMAT, DATA_FORMAT_FULL_RES_16G]),
                // Enter measurement mode.
                T::write(ADDR, vec![REG_POWER_CTL, POWER_CTL_MEASURE]),
                // X = 256, Y = -256, Z = 128.
                T::write_read(
                    ADDR,
                    vec![REG_DATAX0],
                    vec![0x00, 0x01, 0x00, 0xFF, 0x80, 0x00],
                ),
            ];

            let mut mock = Mock::new(&transactions);

            let mut driver = Adxl345::new(&Adxl345Config::default());

            driver.init(&mut mock).await.unwrap();

            let sample = driver.sample(&mut mock).await.unwrap();

            assert!((sample.x - 0.9984).abs() < 0.001);
            assert!((sample.y + 0.9984).abs() < 0.001);
            assert!((sample.z - 0.4992).abs() < 0.001);

            mock.done();
        });
    }

    #[test]
    fn rejects_wrong_device_id() {
        futures::executor::block_on(async {
            let transactions = [T::write_read(ADDR, vec![REG_DEVID], vec![0x00])];

            let mut mock = Mock::new(&transactions);
            let mut driver = Adxl345::new(&Adxl345Config::default());

            let result = driver.init(&mut mock).await;

            assert!(matches!(result, Err(Adxl345Error::InvalidDeviceId(0x00))));

            mock.done();
        });
    }
}
