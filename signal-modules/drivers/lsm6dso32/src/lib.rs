//! `STMicroelectronics` LSM6DSO32 6-axis IMU driver.
//!
//! 3-axis accelerometer, 3-axis gyroscope and an on-chip temperature sensor,
//! accessed over 4-wire SPI. Accelerations are reported in g, angular rates in
//! degrees per second (dps), and temperature in degrees Celsius, all converted
//! from the raw 16-bit registers using the datasheet sensitivity figures
//! (datasheet §4.1 / Table 2).
//!
//! # Configuration
//!
//! [`Lsm6dso32Config`] exposes the datasheet control knobs:
//!
//! - **Accelerometer output data rate** (`accel_odr`) and **gyroscope output
//!   data rate** (`gyro_odr`) - see [`Odr`]. Both blocks run continuously so
//!   [`Lsm6dso32::sample`] can return the latest reading without arming a
//!   conversion on every call.
//! - **Accelerometer full scale** (`accel_scale`) - see [`AccelScale`]. Sets
//!   the measurable range (and therefore the resolution) of the accelerometer.
//! - **Gyroscope full scale** (`gyro_scale`) - see [`GyroScale`]. Sets the
//!   measurable range of the gyroscope.
//!
//! Defaults are a general-purpose motion profile: both blocks at 104 Hz, the
//! accelerometer at the LSM6DSO32-native minimum of +/-4 g, and the gyroscope
//! at +/-250 dps.
//!
//! # Bus
//!
//! The sensor uses SPI mode 3 (CPOL=1, CPHA=1) with the read/write flag in the
//! MSB of the register address. Clock speed and chip-select are configured in
//! the board manifest, not in this driver.
//!
//! # Timing
//!
//! [`Lsm6dso32::init`] issues a software reset and waits for the mandatory
//! boot time before configuring the device (handled with `embassy_time` under
//! `#[cfg(not(test))]`; host tests skip the wait). [`Lsm6dso32::sample`] is
//! non-blocking - it burst-reads the latest data registers with block-data-
//! update enabled so a sample is never torn across an update.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::spi::SpiDevice;

// `embassy_time` is only awaited under `#[cfg(not(test))]`; keep it referenced in
// test builds so it never trips `unused_crate_dependencies`.
#[cfg(test)]
use embassy_time as _;

// Register map (datasheet §9). Only the registers the driver touches.
const REG_WHO_AM_I: u8 = 0x0F;
const REG_CTRL1_XL: u8 = 0x10;
const REG_CTRL2_G: u8 = 0x11;
const REG_CTRL3_C: u8 = 0x12;
const REG_OUT_TEMP_L: u8 = 0x20; // TEMP, GYRO X/Y/Z, ACCEL X/Y/Z are contiguous

// WHO_AM_I fixed value shared by the LSM6DSO family (datasheet §9.10).
const WHO_AM_I: u8 = 0x6C;

// Extra WHO_AM_I read attempts after the first, to ride out the cold-boot
// I2C/SPI interface switch that makes the initial read return a wrong id or
// error.
const WHO_AM_I_RETRIES: u32 = 2;

// SPI read/write flag: MSB of the address byte is 1 for reads (datasheet §6.2.1).
const SPI_READ: u8 = 0x80;

// CTRL3_C bits (datasheet §9.13).
const CTRL3_C_SW_RESET: u8 = 0x01; // software reset, self-clearing
const CTRL3_C_IF_INC: u8 = 0x04; // auto-increment register address on burst
const CTRL3_C_BDU: u8 = 0x40; // block data update: output regs frozen until read

// TEMP: 256 LSB/degC, 0 LSB at 25 degC (datasheet §4.3).
const TEMP_SENS_LSB_PER_C: f32 = 256.0;
const TEMP_ZERO_C: f32 = 25.0;

/// Accelerometer / gyroscope output data rate (datasheet Table 55 / 57).
///
/// The same field encoding drives both blocks. [`Odr::PowerDown`] disables a
/// block; a block left powered down reports a constant reading.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Odr {
    /// Block powered down (no new samples).
    PowerDown = 0b0000,
    /// 12.5 Hz.
    Hz12_5 = 0b0001,
    /// 26 Hz.
    Hz26 = 0b0010,
    /// 52 Hz.
    Hz52 = 0b0011,
    /// 104 Hz (default).
    Hz104 = 0b0100,
    /// 208 Hz.
    Hz208 = 0b0101,
    /// 416 Hz.
    Hz416 = 0b0110,
    /// 833 Hz.
    Hz833 = 0b0111,
    /// 1.66 kHz.
    Khz1_66 = 0b1000,
    /// 3.33 kHz.
    Khz3_33 = 0b1001,
    /// 6.66 kHz.
    Khz6_66 = 0b1010,
}

/// Accelerometer full scale (datasheet Table 55).
///
/// The LSM6DSO32 uses a shifted encoding relative to the LSM6DSO, so the
/// register bits are not the enum discriminant; both are derived internally.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelScale {
    /// +/-4 g (0.122 mg/LSB).
    G4 = 0,
    /// +/-8 g (0.244 mg/LSB).
    G8 = 1,
    /// +/-16 g (0.488 mg/LSB).
    G16 = 2,
    /// +/-32 g (0.976 mg/LSB).
    G32 = 3,
}

/// Gyroscope full scale (datasheet Table 57).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GyroScale {
    /// +/-125 dps (4.375 mdps/LSB).
    Dps125 = 0,
    /// +/-250 dps (8.75 mdps/LSB).
    Dps250 = 1,
    /// +/-500 dps (17.5 mdps/LSB).
    Dps500 = 2,
    /// +/-1000 dps (35 mdps/LSB).
    Dps1000 = 3,
    /// +/-2000 dps (70 mdps/LSB).
    Dps2000 = 4,
}

/// Driver configuration.
///
/// All knobs map to LSM6DSO32 control registers documented in datasheet §9.
/// `Default` is a general-purpose motion profile (104 Hz on both blocks,
/// +/-4 g, +/-250 dps).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lsm6dso32Config {
    /// Accelerometer output data rate.
    pub accel_odr: Odr,
    /// Gyroscope output data rate.
    pub gyro_odr: Odr,
    /// Accelerometer full-scale range.
    pub accel_scale: AccelScale,
    /// Gyroscope full-scale range.
    pub gyro_scale: GyroScale,
}

impl Default for Lsm6dso32Config {
    /// General-purpose motion profile: 104 Hz on both blocks, +/-4 g,
    /// +/-250 dps.
    fn default() -> Self {
        Self {
            accel_odr: Odr::Hz104,
            gyro_odr: Odr::Hz104,
            accel_scale: AccelScale::G4,
            gyro_scale: GyroScale::Dps250,
        }
    }
}

/// One full set of compensated sensor readings.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lsm6dso32Readings {
    /// Acceleration along X in g.
    pub accel_x: f32,
    /// Acceleration along Y in g.
    pub accel_y: f32,
    /// Acceleration along Z in g.
    pub accel_z: f32,
    /// Angular rate around X in degrees per second.
    pub gyro_x: f32,
    /// Angular rate around Y in degrees per second.
    pub gyro_y: f32,
    /// Angular rate around Z in degrees per second.
    pub gyro_z: f32,
    /// Die temperature in degrees Celsius.
    pub temperature: f32,
}

/// Errors returned by the LSM6DSO32 driver.
#[non_exhaustive]
#[derive(Debug)]
pub enum Lsm6dso32Error<E: core::fmt::Debug> {
    /// Underlying SPI bus error.
    Bus(E),
    /// `WHO_AM_I` register did not return `0x6C` - likely wrong device or wiring.
    InvalidId(u8),
}

impl<E: core::fmt::Debug> From<E> for Lsm6dso32Error<E> {
    fn from(e: E) -> Self {
        Self::Bus(e)
    }
}

/// LSM6DSO32 driver instance.
///
/// Construct with [`Lsm6dso32::new`], bring the sensor up with
/// [`Lsm6dso32::init`], then read with [`Lsm6dso32::sample`].
pub struct Lsm6dso32 {
    cfg: Lsm6dso32Config,
}

impl Lsm6dso32 {
    /// Construct a driver instance without touching the bus.
    ///
    /// The sensor is **not** initialised yet - call [`Lsm6dso32::init`] before
    /// [`Lsm6dso32::sample`].
    #[must_use]
    pub fn new(cfg: &Lsm6dso32Config) -> Self {
        Self { cfg: *cfg }
    }

    /// (Re-)initialise the sensor: verify `WHO_AM_I`, software-reset, enable
    /// block-data-update plus address auto-increment, then apply the configured
    /// accelerometer and gyroscope output data rate and full scale.
    ///
    /// Safe to call repeatedly - the generated source task re-runs `init` to
    /// recover a sensor that started failing. A failed re-init leaves the
    /// previous configuration on the device untouched past the point it failed.
    ///
    /// # Errors
    ///
    /// - [`Lsm6dso32Error::Bus`] on any SPI transaction failure.
    /// - [`Lsm6dso32Error::InvalidId`] if `WHO_AM_I` does not read `0x6C`.
    pub async fn init<S: SpiDevice>(
        &mut self,
        spi: &mut S,
    ) -> Result<(), Lsm6dso32Error<S::Error>> {
        let cfg = self.cfg;

        // Cold boot leaves the sensor briefly switching its interface between
        // I2C and SPI, so the first WHO_AM_I read comes back with a wrong id
        // (a garbage or zero value) or errors. Retry twice before giving up
        // (three attempts total).
        let mut attempt = 0;
        loop {
            match read_reg(spi, REG_WHO_AM_I).await {
                Ok(id) if id == WHO_AM_I => break,
                Ok(id) if attempt >= WHO_AM_I_RETRIES => {
                    return Err(Lsm6dso32Error::InvalidId(id));
                }
                Err(err) if attempt >= WHO_AM_I_RETRIES => return Err(err.into()),
                _ => {}
            }
            attempt += 1;
            #[cfg(not(test))]
            embassy_time::Timer::after_millis(10).await;
        }

        // Software reset restores the register defaults, then wait for boot.
        write_reg(spi, REG_CTRL3_C, CTRL3_C_SW_RESET).await?;
        #[cfg(not(test))]
        embassy_time::Timer::after_millis(10).await;

        write_reg(spi, REG_CTRL3_C, CTRL3_C_BDU | CTRL3_C_IF_INC).await?;
        write_reg(spi, REG_CTRL1_XL, ctrl1_xl(cfg.accel_odr, cfg.accel_scale)).await?;
        write_reg(spi, REG_CTRL2_G, ctrl2_g(cfg.gyro_odr, cfg.gyro_scale)).await?;

        log::info!("[lsm6dso32] init OK");

        Ok(())
    }

    /// Burst-read the latest temperature, gyroscope and accelerometer samples
    /// and convert them to physical units. Non-blocking: both blocks run
    /// continuously at the rate configured in [`Lsm6dso32::init`], so the data
    /// registers are always current, and block-data-update guarantees the burst
    /// is internally consistent.
    ///
    /// # Errors
    ///
    /// [`Lsm6dso32Error::Bus`] on any SPI transaction failure.
    pub async fn sample<S: SpiDevice>(
        &mut self,
        spi: &mut S,
    ) -> Result<Lsm6dso32Readings, Lsm6dso32Error<S::Error>> {
        // OUT_TEMP_L..OUTZ_H_A: temp(2) + gyro(6) + accel(6) = 14 contiguous bytes.
        let mut raw = [0u8; 14];
        read_burst(spi, REG_OUT_TEMP_L, &mut raw).await?;

        let temp_raw = i16::from_le_bytes([raw[0], raw[1]]);
        let gx = i16::from_le_bytes([raw[2], raw[3]]);
        let gy = i16::from_le_bytes([raw[4], raw[5]]);
        let gz = i16::from_le_bytes([raw[6], raw[7]]);
        let ax = i16::from_le_bytes([raw[8], raw[9]]);
        let ay = i16::from_le_bytes([raw[10], raw[11]]);
        let az = i16::from_le_bytes([raw[12], raw[13]]);

        let a_scale = self.cfg.accel_scale.sensitivity_mg() / 1000.0; // mg/LSB -> g/LSB
        let g_scale = self.cfg.gyro_scale.sensitivity_mdps() / 1000.0; // mdps/LSB -> dps/LSB

        let readings = Lsm6dso32Readings {
            accel_x: f32::from(ax) * a_scale,
            accel_y: f32::from(ay) * a_scale,
            accel_z: f32::from(az) * a_scale,
            gyro_x: f32::from(gx) * g_scale,
            gyro_y: f32::from(gy) * g_scale,
            gyro_z: f32::from(gz) * g_scale,
            temperature: f32::from(temp_raw) / TEMP_SENS_LSB_PER_C + TEMP_ZERO_C,
        };

        log::debug!(
            "[lsm6dso32] a=({:.2},{:.2},{:.2})g g=({:.1},{:.1},{:.1})dps T={:.1}C",
            readings.accel_x,
            readings.accel_y,
            readings.accel_z,
            readings.gyro_x,
            readings.gyro_y,
            readings.gyro_z,
            readings.temperature
        );

        Ok(readings)
    }
}

impl AccelScale {
    /// `FS_XL` field (`CTRL1_XL` bits 3:2). The LSM6DSO32 mapping is shifted:
    /// 00 = +/-4 g, 01 = +/-32 g, 10 = +/-8 g, 11 = +/-16 g.
    fn fs_bits(self) -> u8 {
        match self {
            AccelScale::G4 => 0b00,
            AccelScale::G32 => 0b01,
            AccelScale::G8 => 0b10,
            AccelScale::G16 => 0b11,
        }
    }

    /// Sensitivity in mg/LSB (datasheet Table 2).
    fn sensitivity_mg(self) -> f32 {
        match self {
            AccelScale::G4 => 0.122,
            AccelScale::G8 => 0.244,
            AccelScale::G16 => 0.488,
            AccelScale::G32 => 0.976,
        }
    }
}

impl GyroScale {
    /// Lower nibble of `CTRL2_G`: `FS_G` (bits 3:2) with `FS_125` (bit 1).
    fn fs_bits(self) -> u8 {
        match self {
            GyroScale::Dps125 => 0b0010,  // FS_125 = 1
            GyroScale::Dps250 => 0b0000,  // FS_G = 00
            GyroScale::Dps500 => 0b0100,  // FS_G = 01
            GyroScale::Dps1000 => 0b1000, // FS_G = 10
            GyroScale::Dps2000 => 0b1100, // FS_G = 11
        }
    }

    /// Sensitivity in mdps/LSB (datasheet Table 2).
    fn sensitivity_mdps(self) -> f32 {
        match self {
            GyroScale::Dps125 => 4.375,
            GyroScale::Dps250 => 8.75,
            GyroScale::Dps500 => 17.5,
            GyroScale::Dps1000 => 35.0,
            GyroScale::Dps2000 => 70.0,
        }
    }
}

/// Compose `CTRL1_XL`: `ODR_XL` in bits 7:4, `FS_XL` in bits 3:2.
fn ctrl1_xl(odr: Odr, scale: AccelScale) -> u8 {
    ((odr as u8) << 4) | (scale.fs_bits() << 2)
}

/// Compose `CTRL2_G`: `ODR_G` in bits 7:4, `FS_G`/`FS_125` in bits 3:1.
fn ctrl2_g(odr: Odr, scale: GyroScale) -> u8 {
    ((odr as u8) << 4) | scale.fs_bits()
}

/// Read a single register (address byte with the read flag, one dummy clock).
async fn read_reg<S: SpiDevice>(spi: &mut S, reg: u8) -> Result<u8, S::Error> {
    let mut rx = [0u8; 2];
    spi.transfer(&mut rx, &[reg | SPI_READ, 0x00]).await?;

    Ok(rx[1])
}

/// Read `buf.len()` consecutive registers starting at `reg` (address
/// auto-increments on-chip when `IF_INC` is set).
async fn read_burst<S: SpiDevice>(spi: &mut S, reg: u8, buf: &mut [u8]) -> Result<(), S::Error> {
    // One extra leading byte carries the address; its echo is discarded.
    let mut tx = [0u8; 15];
    let mut rx = [0u8; 15];
    tx[0] = reg | SPI_READ;
    let len = buf.len() + 1;
    spi.transfer(&mut rx[..len], &tx[..len]).await?;
    buf.copy_from_slice(&rx[1..len]);

    Ok(())
}

/// Write a single register (address byte with the read flag cleared).
async fn write_reg<S: SpiDevice>(spi: &mut S, reg: u8, val: u8) -> Result<(), S::Error> {
    spi.write(&[reg & !SPI_READ, val]).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use embedded_hal_mock::eh1::spi::{Mock, Transaction as T};

    fn read_reg_txns(reg: u8, val: u8) -> Vec<T<u8>> {
        vec![
            T::transaction_start(),
            T::transfer(vec![reg | SPI_READ, 0x00], vec![0x00, val]),
            T::transaction_end(),
        ]
    }

    fn write_reg_txns(reg: u8, val: u8) -> Vec<T<u8>> {
        vec![
            T::transaction_start(),
            T::write_vec(vec![reg, val]),
            T::transaction_end(),
        ]
    }

    fn init_transactions(cfg: Lsm6dso32Config) -> Vec<T<u8>> {
        let mut v = read_reg_txns(REG_WHO_AM_I, WHO_AM_I);
        v.extend(write_reg_txns(REG_CTRL3_C, CTRL3_C_SW_RESET));
        v.extend(write_reg_txns(REG_CTRL3_C, CTRL3_C_BDU | CTRL3_C_IF_INC));
        v.extend(write_reg_txns(
            REG_CTRL1_XL,
            ctrl1_xl(cfg.accel_odr, cfg.accel_scale),
        ));
        v.extend(write_reg_txns(
            REG_CTRL2_G,
            ctrl2_g(cfg.gyro_odr, cfg.gyro_scale),
        ));

        v
    }

    // A device at rest, tilted so ~1 g sits on Z, with a small rotation and a
    // die temperature of ~27 degC. Raw counts use the default scales.
    fn sample_bytes() -> (Vec<u8>, Vec<u8>) {
        let temp = 512i16; // (27 - 25) degC * 256 LSB/degC
        let gx = 100i16; // 100 * 8.75 mdps = 0.875 dps
        let gy = -50i16;
        let gz = 200i16;
        let ax = 20i16;
        let ay = -30i16;
        let az = 8197i16; // 8197 * 0.122 mg ~= 1.0 g

        let mut data = Vec::new();
        for v in [temp, gx, gy, gz, ax, ay, az] {
            data.extend_from_slice(&v.to_le_bytes());
        }

        let mut tx = vec![REG_OUT_TEMP_L | SPI_READ];
        tx.extend(core::iter::repeat_n(0u8, data.len()));
        let mut rx = vec![0u8];
        rx.extend_from_slice(&data);

        (tx, rx)
    }

    fn sample_txns() -> Vec<T<u8>> {
        let (tx, rx) = sample_bytes();

        vec![
            T::transaction_start(),
            T::transfer(tx, rx),
            T::transaction_end(),
        ]
    }

    #[test]
    fn init_and_sample() {
        futures::executor::block_on(async {
            let cfg = Lsm6dso32Config::default();
            let mut txns = init_transactions(cfg);
            txns.extend(sample_txns());
            let mut spi = Mock::new(&txns);

            let mut driver = Lsm6dso32::new(&cfg);
            driver.init(&mut spi).await.unwrap();
            let r = driver.sample(&mut spi).await.unwrap();

            assert!(
                r.temperature > 20.0 && r.temperature < 35.0,
                "T={}",
                r.temperature
            );
            assert!((r.accel_z - 1.0).abs() < 0.1, "az={}", r.accel_z);
            assert!(r.accel_x.abs() < 0.1 && r.accel_y.abs() < 0.1, "ax/ay off");
            assert!(r.gyro_x.abs() < 5.0 && r.gyro_z.abs() < 5.0, "gyro off");

            spi.done();
        });
    }

    #[test]
    fn wrong_chip_id_returns_error() {
        futures::executor::block_on(async {
            // A cleanly-read wrong id is retried like a failed cold-boot read,
            // so init reads WHO_AM_I `WHO_AM_I_RETRIES + 1` times before failing.
            let mut txns = Vec::new();
            for _ in 0..=WHO_AM_I_RETRIES {
                txns.extend(read_reg_txns(REG_WHO_AM_I, 0x00));
            }
            let mut spi = Mock::new(&txns);

            let mut driver = Lsm6dso32::new(&Lsm6dso32Config::default());
            let result = driver.init(&mut spi).await;
            assert!(matches!(result, Err(Lsm6dso32Error::InvalidId(0x00))));

            spi.done();
        });
    }

    #[test]
    fn custom_scales_are_written_to_registers() {
        futures::executor::block_on(async {
            let cfg = Lsm6dso32Config {
                accel_odr: Odr::Hz416,
                gyro_odr: Odr::Hz833,
                accel_scale: AccelScale::G16,
                gyro_scale: GyroScale::Dps2000,
            };
            // CTRL1_XL: (0b0110 << 4) | (0b11 << 2) = 0x6C
            assert_eq!(ctrl1_xl(cfg.accel_odr, cfg.accel_scale), 0x6C);
            // CTRL2_G: (0b0111 << 4) | 0b1100 = 0x7C
            assert_eq!(ctrl2_g(cfg.gyro_odr, cfg.gyro_scale), 0x7C);

            let mut txns = init_transactions(cfg);
            txns.extend(sample_txns());
            let mut spi = Mock::new(&txns);

            let mut driver = Lsm6dso32::new(&cfg);
            driver.init(&mut spi).await.unwrap();
            let _ = driver.sample(&mut spi).await.unwrap();

            spi.done();
        });
    }

    #[test]
    fn reinit_recovers_after_first_bring_up() {
        // The generated source task re-runs init() to recover a degraded sensor.
        // Two full bring-up sequences back-to-back must both succeed and leave
        // the driver able to sample.
        futures::executor::block_on(async {
            let cfg = Lsm6dso32Config::default();
            let mut txns = init_transactions(cfg);
            txns.extend(init_transactions(cfg)); // recovery re-init
            txns.extend(sample_txns());
            let mut spi = Mock::new(&txns);

            let mut driver = Lsm6dso32::new(&cfg);
            driver.init(&mut spi).await.unwrap();
            driver.init(&mut spi).await.unwrap(); // recovery re-init
            let r = driver.sample(&mut spi).await.unwrap();
            assert!((r.accel_z - 1.0).abs() < 0.1, "az={}", r.accel_z);

            spi.done();
        });
    }
}
