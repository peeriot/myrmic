//! Bosch BME280 environmental sensor driver.
//!
//! Combined temperature, pressure, and humidity sensor accessed over I2C.
//! Readings are produced in °C, hPa, and %RH using the on-chip calibration
//! table and the datasheet's double-precision compensation formulas (§4.2.3).
//!
//! # Configuration
//!
//! [`Bme280Config`] exposes the datasheet-aligned knobs:
//!
//! - **Per-channel oversampling** (`osrs_t`, `osrs_p`, `osrs_h`) — see
//!   [`Oversampling`]. Higher oversampling reduces noise at the cost of
//!   longer conversion time.
//! - **IIR filter coefficient** (`filter`) — see [`Filter`]. Smooths
//!   short-term fluctuations on temperature and pressure.
//! - **Standby time** (`t_sb`) — see [`Standby`]. Sets the inactive period
//!   between samples in Normal mode.
//!
//! Defaults match a "weather station" profile (×1 oversampling on all
//! channels, IIR filter off, 0.5 ms standby). After construction the sensor
//! runs in Normal mode (continuous sampling).
//!
//! # Timing
//!
//! [`Bme280::sample`] is non-blocking — it issues a single burst read of the
//! latest data registers. The caller's sample loop owns the sample interval;
//! Normal mode keeps the data registers current between samples.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

const REG_CHIP_ID: u8 = 0xD0;
const CHIP_ID: u8 = 0x60;
const REG_CTRL_HUM: u8 = 0xF2;
const REG_CTRL_MEAS: u8 = 0xF4;
const REG_CONFIG: u8 = 0xF5;
const REG_PRESS_MSB: u8 = 0xF7;
const REG_CALIB_TP: u8 = 0x88; // 24 bytes: dig_T1-T3, dig_P1-P9
const REG_CALIB_H1: u8 = 0xA1; // 1 byte:  dig_H1
const REG_CALIB_H2: u8 = 0xE1; // 7 bytes: dig_H2-H6

// Normal mode (continuous sampling) — embedded into `ctrl_meas` alongside the
// oversampling fields. Forced/Sleep modes are not exposed; the driver always
// runs Normal so that [`Bme280::sample`] can return the latest reading
// without arming a measurement on each call.
const MODE_NORMAL: u8 = 0b11;

/// Per-channel oversampling — trades noise reduction for longer conversion time.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Oversampling {
    /// Skip the channel (output fixed at 0x80000, datasheet §5.4.4/5.4.5).
    Skip = 0,
    /// ×1 oversampling (no averaging).
    X1 = 1,
    /// ×2 oversampling.
    X2 = 2,
    /// ×4 oversampling.
    X4 = 3,
    /// ×8 oversampling.
    X8 = 4,
    /// ×16 oversampling.
    X16 = 5,
}

/// IIR filter coefficient — smooths short-term fluctuations on temperature and pressure.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Filter off (no smoothing).
    Off = 0,
    /// IIR coefficient ×2.
    X2 = 1,
    /// IIR coefficient ×4.
    X4 = 2,
    /// IIR coefficient ×8.
    X8 = 3,
    /// IIR coefficient ×16.
    X16 = 4,
}

/// Normal-mode standby time — inactive period between measurements.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standby {
    /// 0.5 ms standby.
    Ms0_5 = 0,
    /// 62.5 ms standby.
    Ms62_5 = 1,
    /// 125 ms standby.
    Ms125 = 2,
    /// 250 ms standby.
    Ms250 = 3,
    /// 500 ms standby.
    Ms500 = 4,
    /// 1000 ms standby.
    Ms1000 = 5,
    /// 10 ms standby.
    Ms10 = 6,
    /// 20 ms standby.
    Ms20 = 7,
}

/// Driver configuration.
///
/// All knobs map to BME280 control registers documented in datasheet §5.4.
/// `Default` matches a weather-station profile (×1 oversampling, IIR off,
/// 0.5 ms standby).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bme280Config {
    /// I2C address. Factory default `0x76` (SDO tied to GND); `0x77` when
    /// SDO is tied to VDDIO.
    pub i2c_addr: u8,
    /// Temperature oversampling.
    pub osrs_t: Oversampling,
    /// Pressure oversampling.
    pub osrs_p: Oversampling,
    /// Humidity oversampling.
    pub osrs_h: Oversampling,
    /// IIR filter coefficient.
    pub filter: Filter,
    /// Normal-mode inactive period between samples.
    pub t_sb: Standby,
}

impl Default for Bme280Config {
    /// Weather-station profile: ×1 oversampling on all channels, IIR off,
    /// 0.5 ms standby, address `0x76`.
    fn default() -> Self {
        Self {
            i2c_addr: 0x76,
            osrs_t: Oversampling::X1,
            osrs_p: Oversampling::X1,
            osrs_h: Oversampling::X1,
            filter: Filter::Off,
            t_sb: Standby::Ms0_5,
        }
    }
}

/// One full set of compensated sensor readings.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bme280Readings {
    /// Temperature in degrees Celsius (resolution ~0.01°C).
    pub temperature: f32,
    /// Atmospheric pressure in hPa (resolution depends on oversampling).
    pub pressure: f32,
    /// Relative humidity in percent (clamped to 0..=100).
    pub humidity: f32,
}

/// Errors returned by the BME280 driver.
#[non_exhaustive]
#[derive(Debug)]
pub enum Bme280Error<E: core::fmt::Debug> {
    /// Underlying I2C bus error.
    Bus(E),
    /// Chip ID register did not return `0x60` — likely wrong device or wiring.
    InvalidId(u8),
    /// `dig_T1` or `dig_P1` read as `0` or `0xFFFF`. Neither is a programmed
    /// trim value, so the calibration transfer did not reach the chip's NVM.
    InvalidCalibration,
}

impl<E: core::fmt::Debug> From<E> for Bme280Error<E> {
    fn from(e: E) -> Self {
        Self::Bus(e)
    }
}

#[derive(Default)]
struct Calibration {
    dig_t1: u16,
    dig_t2: i16,
    dig_t3: i16,
    dig_p1: u16,
    dig_p2: i16,
    dig_p3: i16,
    dig_p4: i16,
    dig_p5: i16,
    dig_p6: i16,
    dig_p7: i16,
    dig_p8: i16,
    dig_p9: i16,
    dig_h1: u8,
    dig_h2: i16,
    dig_h3: u8,
    dig_h4: i16,
    dig_h5: i16,
    dig_h6: i8,
}

/// BME280 driver instance.
///
/// Construct with [`Bme280::new`], bring the sensor up with [`Bme280::init`],
/// then read with [`Bme280::sample`].
pub struct Bme280 {
    cfg: Bme280Config,
    cal: Calibration,
}

impl Bme280 {
    /// Construct a driver instance without touching the bus.
    ///
    /// The sensor is **not** initialised yet — call [`Bme280::init`] before
    /// [`Bme280::sample`]. Calibration is zeroed until `init` loads it.
    #[must_use]
    pub fn new(cfg: &Bme280Config) -> Self {
        Self {
            cfg: *cfg,
            cal: Calibration::default(),
        }
    }

    /// (Re-)initialise the sensor: verify the chip ID, load the calibration
    /// table, and apply the configured oversampling / filter / standby. Leaves
    /// the sensor in Normal (continuous) mode.
    ///
    /// Safe to call repeatedly — the generated source task re-runs `init` to
    /// recover a sensor that started failing. The stored calibration is only
    /// replaced once a full read succeeds, so a failed re-init leaves the
    /// previous calibration intact.
    ///
    /// # Errors
    ///
    /// - [`Bme280Error::Bus`] on any I2C transaction failure.
    /// - [`Bme280Error::InvalidId`] if the chip ID register does not read `0x60`.
    /// - [`Bme280Error::InvalidCalibration`] if `dig_T1` or `dig_P1` reads `0`
    ///   or `0xFFFF`.
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), Bme280Error<I::Error>> {
        let cfg = self.cfg;
        let addr = cfg.i2c_addr;

        let mut id = [0u8; 1];
        bus.write_read(addr, &[REG_CHIP_ID], &mut id).await?;
        if id[0] != CHIP_ID {
            return Err(Bme280Error::InvalidId(id[0]));
        }

        // Read temp+pressure calibration (24 bytes: 0x88–0x9F)
        let mut tp = [0u8; 24];
        bus.write_read(addr, &[REG_CALIB_TP], &mut tp).await?;
        // The two unsigned words are never programmed as 0 or 0xFFFF; reading
        // either means the transfer returned bus idle levels, not the NVM.
        let dig_t1 = u16::from_le_bytes([tp[0], tp[1]]);
        let dig_p1 = u16::from_le_bytes([tp[6], tp[7]]);
        if matches!(dig_t1, 0x0000 | 0xFFFF) || matches!(dig_p1, 0x0000 | 0xFFFF) {
            return Err(Bme280Error::InvalidCalibration);
        }

        // Read H1 (1 byte: 0xA1)
        let mut h1 = [0u8; 1];
        bus.write_read(addr, &[REG_CALIB_H1], &mut h1).await?;

        // Read H2–H6 (7 bytes: 0xE1–0xE7)
        let mut h2_6 = [0u8; 7];
        bus.write_read(addr, &[REG_CALIB_H2], &mut h2_6).await?;

        let cal = Calibration {
            dig_t1,
            dig_t2: i16::from_le_bytes([tp[2], tp[3]]),
            dig_t3: i16::from_le_bytes([tp[4], tp[5]]),
            dig_p1,
            dig_p2: i16::from_le_bytes([tp[8], tp[9]]),
            dig_p3: i16::from_le_bytes([tp[10], tp[11]]),
            dig_p4: i16::from_le_bytes([tp[12], tp[13]]),
            dig_p5: i16::from_le_bytes([tp[14], tp[15]]),
            dig_p6: i16::from_le_bytes([tp[16], tp[17]]),
            dig_p7: i16::from_le_bytes([tp[18], tp[19]]),
            dig_p8: i16::from_le_bytes([tp[20], tp[21]]),
            dig_p9: i16::from_le_bytes([tp[22], tp[23]]),
            dig_h1: h1[0],
            dig_h2: i16::from_le_bytes([h2_6[0], h2_6[1]]),
            dig_h3: h2_6[2],
            // 12-bit signed words: the MSB byte carries the sign (§4.2.2).
            dig_h4: (i16::from(h2_6[3].cast_signed()) << 4) | i16::from(h2_6[4] & 0x0F),
            dig_h5: (i16::from(h2_6[5].cast_signed()) << 4) | i16::from(h2_6[4] >> 4),
            dig_h6: h2_6[6].cast_signed(),
        };

        // Datasheet §5.4.3: ctrl_hum must be written before ctrl_meas takes effect.
        let ctrl_hum = (cfg.osrs_h as u8) & 0b111;
        let config_reg = ((cfg.t_sb as u8) << 5) | ((cfg.filter as u8) << 2);
        let ctrl_meas = ((cfg.osrs_t as u8) << 5) | ((cfg.osrs_p as u8) << 2) | MODE_NORMAL;
        bus.write(addr, &[REG_CTRL_HUM, ctrl_hum]).await?;
        bus.write(addr, &[REG_CONFIG, config_reg]).await?;
        bus.write(addr, &[REG_CTRL_MEAS, ctrl_meas]).await?;

        self.cal = cal;
        log::info!("[bme280] init OK at 0x{addr:02X}");
        Ok(())
    }

    /// Read the latest temperature, pressure, and humidity in one burst.
    /// Does not block waiting for a fresh sample — the driver runs the
    /// sensor in Normal mode so the data registers are updated continuously
    /// at the standby+conversion cadence configured in [`Bme280::init`].
    ///
    /// # Errors
    ///
    /// [`Bme280Error::Bus`] on any I2C transaction failure.
    pub async fn sample<I: I2c>(
        &mut self,
        bus: &mut I,
    ) -> Result<Bme280Readings, Bme280Error<I::Error>> {
        // Read press(3) + temp(3) + hum(2) = 8 bytes starting at 0xF7
        let mut raw = [0u8; 8];
        bus.write_read(self.cfg.i2c_addr, &[REG_PRESS_MSB], &mut raw)
            .await?;

        let adc_p = (i32::from(raw[0])) << 12 | (i32::from(raw[1])) << 4 | (i32::from(raw[2])) >> 4;
        let adc_t = (i32::from(raw[3])) << 12 | (i32::from(raw[4])) << 4 | (i32::from(raw[5])) >> 4;
        let adc_h = (i32::from(raw[6])) << 8 | i32::from(raw[7]);

        let (t_fine, temperature) = compensate_temperature(adc_t, &self.cal);
        let pressure = compensate_pressure(adc_p, t_fine, &self.cal);
        let humidity = compensate_humidity(adc_h, t_fine, &self.cal);

        log::debug!("[bme280] T={temperature:.1}°C P={pressure:.1}hPa H={humidity:.1}%");
        Ok(Bme280Readings {
            temperature,
            pressure,
            humidity,
        })
    }
}

// Compensation (Bosch BME280 datasheet §4.2.3 double-precision formulas)

// Bosch BME280 §4.2.3 double-precision formula: intermediate math is f64; the final
// f64→f32 truncation at the output boundary is intentional (sensor resolution is ~0.01°C).
#[allow(clippy::cast_possible_truncation)]
fn compensate_temperature(adc_t: i32, cal: &Calibration) -> (f64, f32) {
    let t = f64::from(adc_t);
    let var1 = t / 16384.0 - f64::from(cal.dig_t1) / 1024.0;
    let var1 = var1 * f64::from(cal.dig_t2);
    let var2 = t / 131_072.0 - f64::from(cal.dig_t1) / 8192.0;
    let var2 = var2 * var2 * f64::from(cal.dig_t3);
    let t_fine = var1 + var2;
    (t_fine, (t_fine / 5120.0) as f32)
}

// Same §4.2.3 formula; f64→f32 at the Pa→hPa output boundary is intentional.
#[allow(clippy::cast_possible_truncation)]
fn compensate_pressure(adc_p: i32, t_fine: f64, cal: &Calibration) -> f32 {
    let p = f64::from(adc_p);
    let var1 = t_fine / 2.0 - 64000.0;
    let var2 = var1 * var1 * f64::from(cal.dig_p6) / 32768.0;
    let var2 = var2 + var1 * f64::from(cal.dig_p5) * 2.0;
    let var2 = var2 / 4.0 + f64::from(cal.dig_p4) * 65536.0;
    let var3 = f64::from(cal.dig_p3) * var1 * var1 / 524_288.0;
    let var1 = (var3 + f64::from(cal.dig_p2) * var1) / 524_288.0;
    let var1 = (1.0 + var1 / 32768.0) * f64::from(cal.dig_p1);
    if var1 == 0.0 {
        return 0.0;
    }
    let mut pressure = 1_048_576.0 - p;
    pressure = (pressure - var2 / 4096.0) * 6250.0 / var1;
    let var1 = f64::from(cal.dig_p9) * pressure * pressure / 2_147_483_648.0;
    let var2 = pressure * f64::from(cal.dig_p8) / 32_768.0;
    ((pressure + (var1 + var2 + f64::from(cal.dig_p7)) / 16.0) / 100.0) as f32 // Pa → hPa
}

// Same §4.2.3 formula; f64→f32 at the output boundary is intentional (humidity is clamped 0–100%).
#[allow(clippy::cast_possible_truncation)]
fn compensate_humidity(adc_h: i32, t_fine: f64, cal: &Calibration) -> f32 {
    let h = f64::from(adc_h);
    let x = t_fine - 76800.0;
    let h = (h - (f64::from(cal.dig_h4) * 64.0 + f64::from(cal.dig_h5) / 16384.0 * x))
        * (f64::from(cal.dig_h2) / 65_536.0
            * (1.0
                + f64::from(cal.dig_h6) / 67_108_864.0
                    * x
                    * (1.0 + f64::from(cal.dig_h3) / 67_108_864.0 * x)));
    let h = h * (1.0 - f64::from(cal.dig_h1) * h / 524_288.0);
    h.clamp(0.0, 100.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};

    // Calibration bytes that yield T≈25°C, P≈1013 hPa, H≈50% for the raw values below.
    // Values taken from the Bosch BME280 datasheet example.
    const ADDR: u8 = 0x76;

    // Defaults: osrs_t=X1, osrs_p=X1, mode=Normal → ctrl_meas = 0b001_001_11 = 0x27
    const DEFAULT_CTRL_MEAS: u8 =
        ((Oversampling::X1 as u8) << 5) | ((Oversampling::X1 as u8) << 2) | MODE_NORMAL;
    // Defaults: osrs_h=X1 → ctrl_hum = 0x01
    const DEFAULT_CTRL_HUM: u8 = Oversampling::X1 as u8;
    // Defaults: t_sb=0.5ms, filter=Off → config = 0x00
    const DEFAULT_CONFIG: u8 = ((Standby::Ms0_5 as u8) << 5) | ((Filter::Off as u8) << 2);

    // 7 bytes 0xE1–0xE7: dig_H2=356, dig_H3=0, dig_H4=288, dig_H5=800, dig_H6=30
    const CAL_H2_6: [u8; 7] = [100, 1, 0, 18, 0, 50, 30];
    // Same, with bit 11 set in dig_H4 and dig_H5: 0xE4=0x90 → dig_H4=-1782,
    // 0xE6=0x83 → dig_H5=-2000. Both words are signed on the chip.
    const CAL_H2_6_NEGATIVE: [u8; 7] = [100, 1, 0, 0x90, 0x0A, 0x83, 30];
    const CAL_H1: u8 = 75;

    // 24 bytes: dig_T1=27504, dig_T2=26435, dig_T3=-1000, dig_P1-P9 typical silicon
    fn cal_tp_bytes() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&27504u16.to_le_bytes()); // T1
        v.extend_from_slice(&26435i16.to_le_bytes()); // T2
        v.extend_from_slice(&(-1000i16).to_le_bytes()); // T3
        v.extend_from_slice(&36477u16.to_le_bytes()); // P1
        v.extend_from_slice(&(-10685i16).to_le_bytes()); // P2
        v.extend_from_slice(&3024i16.to_le_bytes()); // P3
        v.extend_from_slice(&2855i16.to_le_bytes()); // P4
        v.extend_from_slice(&140i16.to_le_bytes()); // P5
        v.extend_from_slice(&(-7i16).to_le_bytes()); // P6
        v.extend_from_slice(&15500i16.to_le_bytes()); // P7
        v.extend_from_slice(&(-14600i16).to_le_bytes()); // P8
        v.extend_from_slice(&6000i16.to_le_bytes()); // P9
        v
    }

    fn init_transactions_with(h2_6: [u8; 7]) -> Vec<T> {
        vec![
            T::write_read(ADDR, vec![REG_CHIP_ID], vec![CHIP_ID]),
            T::write_read(ADDR, vec![REG_CALIB_TP], cal_tp_bytes()),
            T::write_read(ADDR, vec![REG_CALIB_H1], vec![CAL_H1]),
            T::write_read(ADDR, vec![REG_CALIB_H2], h2_6.to_vec()),
            T::write(ADDR, vec![REG_CTRL_HUM, DEFAULT_CTRL_HUM]),
            T::write(ADDR, vec![REG_CONFIG, DEFAULT_CONFIG]),
            T::write(ADDR, vec![REG_CTRL_MEAS, DEFAULT_CTRL_MEAS]),
        ]
    }

    fn init_transactions() -> Vec<T> {
        init_transactions_with(CAL_H2_6)
    }

    // Raw pressure/temp/humidity that should decode to ~25°C / ~1013 hPa / ~29%.
    const RAW_P: i32 = 415_148;
    const RAW_T: i32 = 519_888;
    const RAW_H: i32 = 26_214;

    // Data-register byte layout of a raw triple (datasheet §5.4.4–§5.4.6).
    // (x & 0xF) << 4 lies in 0..=240 — fits in u8.
    #[allow(clippy::cast_possible_truncation)]
    fn raw_bytes(adc_p: i32, adc_t: i32, adc_h: i32) -> Vec<u8> {
        let adc_p = adc_p.cast_unsigned();
        let adc_t = adc_t.cast_unsigned();
        let adc_h = adc_h.cast_unsigned();
        vec![
            ((adc_p >> 12) & 0xFF) as u8,
            ((adc_p >> 4) & 0xFF) as u8,
            ((adc_p & 0xF) << 4) as u8,
            ((adc_t >> 12) & 0xFF) as u8,
            ((adc_t >> 4) & 0xFF) as u8,
            ((adc_t & 0xF) << 4) as u8,
            ((adc_h >> 8) & 0xFF) as u8,
            (adc_h & 0xFF) as u8,
        ]
    }

    fn raw_sample_bytes() -> Vec<u8> {
        raw_bytes(RAW_P, RAW_T, RAW_H)
    }

    // Transcription of the datasheet §4.2.3 double-precision compensation as
    // Bosch's `bme280.c` implements it, decoding the calibration from the raw
    // register bytes independently of the driver. `t_fine` is truncated to a
    // 32-bit integer exactly as the reference does.
    mod reference {
        pub struct Cal {
            t1: f64,
            t2: f64,
            t3: f64,
            p: [f64; 9],
            h1: f64,
            h2: f64,
            h3: f64,
            h4: f64,
            h5: f64,
            h6: f64,
        }

        pub fn parse(tp: &[u8], h1: u8, h2_6: [u8; 7]) -> Cal {
            let u = |i: usize| f64::from(u16::from_le_bytes([tp[i], tp[i + 1]]));
            let s = |i: usize| f64::from(i16::from_le_bytes([tp[i], tp[i + 1]]));
            Cal {
                t1: u(0),
                t2: s(2),
                t3: s(4),
                p: [u(6), s(8), s(10), s(12), s(14), s(16), s(18), s(20), s(22)],
                h1: f64::from(h1),
                h2: f64::from(i16::from_le_bytes([h2_6[0], h2_6[1]])),
                h3: f64::from(h2_6[2]),
                h4: f64::from((i16::from(h2_6[3].cast_signed()) * 16) | i16::from(h2_6[4] & 0x0F)),
                h5: f64::from((i16::from(h2_6[5].cast_signed()) * 16) | i16::from(h2_6[4] >> 4)),
                h6: f64::from(h2_6[6].cast_signed()),
            }
        }

        #[allow(clippy::cast_possible_truncation)]
        pub fn temperature(adc_t: i32, c: &Cal) -> (i32, f64) {
            let adc_t = f64::from(adc_t);
            let var1 = (adc_t / 16384.0 - c.t1 / 1024.0) * c.t2;
            let var2 =
                (adc_t / 131_072.0 - c.t1 / 8192.0) * (adc_t / 131_072.0 - c.t1 / 8192.0) * c.t3;
            ((var1 + var2) as i32, (var1 + var2) / 5120.0)
        }

        pub fn pressure(adc_p: i32, t_fine: i32, c: &Cal) -> f64 {
            let [p1, p2, p3, p4, p5, p6, p7, p8, p9] = c.p;
            let var1 = f64::from(t_fine) / 2.0 - 64000.0;
            let var2 = var1 * var1 * p6 / 32768.0;
            let var2 = var2 + var1 * p5 * 2.0;
            let var2 = var2 / 4.0 + p4 * 65536.0;
            let var1 = (p3 * var1 * var1 / 524_288.0 + p2 * var1) / 524_288.0;
            let var1 = (1.0 + var1 / 32768.0) * p1;
            if var1 == 0.0 {
                return 0.0;
            }
            let p = 1_048_576.0 - f64::from(adc_p);
            let p = (p - var2 / 4096.0) * 6250.0 / var1;
            let var1 = p9 * p * p / 2_147_483_648.0;
            let var2 = p * p8 / 32768.0;
            (p + (var1 + var2 + p7) / 16.0) / 100.0
        }

        pub fn humidity(adc_h: i32, t_fine: i32, c: &Cal) -> f64 {
            let var_h = f64::from(t_fine) - 76800.0;
            let var_h = (f64::from(adc_h) - (c.h4 * 64.0 + c.h5 / 16384.0 * var_h))
                * (c.h2 / 65536.0
                    * (1.0 + c.h6 / 67_108_864.0 * var_h * (1.0 + c.h3 / 67_108_864.0 * var_h)));
            let var_h = var_h * (1.0 - c.h1 * var_h / 524_288.0);
            var_h.clamp(0.0, 100.0)
        }
    }

    // The reference truncates `t_fine` to an integer and the driver keeps it
    // as f64; the tolerances absorb that plus the f32 output rounding.
    const T_TOLERANCE: f64 = 0.001;
    const P_TOLERANCE: f64 = 0.01;
    const H_TOLERANCE: f64 = 0.01;

    async fn init_with(h2_6: [u8; 7]) -> (Bme280, Mock) {
        let mut mock = Mock::new(&init_transactions_with(h2_6));
        let cfg = Bme280Config {
            i2c_addr: ADDR,
            ..Bme280Config::default()
        };
        let mut driver = Bme280::new(&cfg);
        driver.init(&mut mock).await.unwrap();
        (driver, mock)
    }

    // Grids over the raw ADC domains; a step of 0x4000 on the 20-bit words and
    // 0x1000 on the 16-bit word spans the operating range many times over.
    fn raw_grid_20bit() -> impl Iterator<Item = i32> {
        (0..=0xF_FFFF).step_by(0x4000).chain([0xF_FFFF])
    }

    fn raw_grid_16bit() -> impl Iterator<Item = i32> {
        (0..=0xFFFF).step_by(0x1000).chain([0xFFFF])
    }

    fn assert_matches_reference(h2_6: [u8; 7]) {
        futures::executor::block_on(async {
            let (driver, mut mock) = init_with(h2_6).await;
            let reference = reference::parse(&cal_tp_bytes(), CAL_H1, h2_6);
            let mut compared = 0usize;
            for adc_t in raw_grid_20bit() {
                let (ref_t_fine, ref_t) = reference::temperature(adc_t, &reference);
                let (t_fine, t) = compensate_temperature(adc_t, &driver.cal);
                assert!(
                    (f64::from(t) - ref_t).abs() < T_TOLERANCE,
                    "adc_t={adc_t:#x}: T={t} vs reference {ref_t}"
                );
                for adc_p in raw_grid_20bit() {
                    let ref_p = reference::pressure(adc_p, ref_t_fine, &reference);
                    let p = compensate_pressure(adc_p, t_fine, &driver.cal);
                    assert!(
                        (f64::from(p) - ref_p).abs() < P_TOLERANCE + 1e-6 * ref_p.abs(),
                        "adc_t={adc_t:#x} adc_p={adc_p:#x}: P={p} vs reference {ref_p}"
                    );
                    compared += 1;
                }
                for adc_h in raw_grid_16bit() {
                    let ref_h = reference::humidity(adc_h, ref_t_fine, &reference);
                    let h = compensate_humidity(adc_h, t_fine, &driver.cal);
                    assert!(
                        (f64::from(h) - ref_h).abs() < H_TOLERANCE,
                        "adc_t={adc_t:#x} adc_h={adc_h:#x}: H={h} vs reference {ref_h}"
                    );
                    compared += 1;
                }
            }
            assert!(compared > 1000, "grid degenerated to {compared} points");
            mock.done();
        });
    }

    #[test]
    fn compensation_matches_reference() {
        assert_matches_reference(CAL_H2_6);
    }

    #[test]
    fn compensation_matches_reference_with_negative_h4_h5() {
        assert_matches_reference(CAL_H2_6_NEGATIVE);
    }

    #[test]
    fn init_rejects_an_all_zero_calibration() {
        futures::executor::block_on(async {
            let mut mock = Mock::new(&[
                T::write_read(ADDR, vec![REG_CHIP_ID], vec![CHIP_ID]),
                T::write_read(ADDR, vec![REG_CALIB_TP], vec![0; 24]),
            ]);
            let cfg = Bme280Config {
                i2c_addr: ADDR,
                ..Bme280Config::default()
            };
            let mut driver = Bme280::new(&cfg);
            let result = driver.init(&mut mock).await;
            assert!(matches!(result, Err(Bme280Error::InvalidCalibration)));
            mock.done();
        });
    }

    #[test]
    fn init_rejects_an_all_ones_pressure_word() {
        futures::executor::block_on(async {
            let mut cal = cal_tp_bytes();
            cal[6..8].copy_from_slice(&0xFFFFu16.to_le_bytes()); // dig_P1
            let mut mock = Mock::new(&[
                T::write_read(ADDR, vec![REG_CHIP_ID], vec![CHIP_ID]),
                T::write_read(ADDR, vec![REG_CALIB_TP], cal),
            ]);
            let cfg = Bme280Config {
                i2c_addr: ADDR,
                ..Bme280Config::default()
            };
            let mut driver = Bme280::new(&cfg);
            let result = driver.init(&mut mock).await;
            assert!(matches!(result, Err(Bme280Error::InvalidCalibration)));
            mock.done();
        });
    }

    #[test]
    fn init_and_sample() {
        futures::executor::block_on(async {
            let mut txns = init_transactions();
            txns.push(T::write_read(ADDR, vec![REG_PRESS_MSB], raw_sample_bytes()));
            let mut mock = Mock::new(&txns);

            let cfg = Bme280Config {
                i2c_addr: ADDR,
                ..Bme280Config::default()
            };
            let mut driver = Bme280::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let readings = driver.sample(&mut mock).await.unwrap();

            let reference = reference::parse(&cal_tp_bytes(), CAL_H1, CAL_H2_6);
            let (t_fine, t) = reference::temperature(RAW_T, &reference);
            let p = reference::pressure(RAW_P, t_fine, &reference);
            let h = reference::humidity(RAW_H, t_fine, &reference);
            assert!(
                (f64::from(readings.temperature) - t).abs() < T_TOLERANCE,
                "T={} vs reference {t}",
                readings.temperature
            );
            assert!(
                (f64::from(readings.pressure) - p).abs() < P_TOLERANCE,
                "P={} vs reference {p}",
                readings.pressure
            );
            assert!(
                (f64::from(readings.humidity) - h).abs() < H_TOLERANCE,
                "H={} vs reference {h}",
                readings.humidity
            );

            mock.done();
        });
    }

    #[test]
    fn wrong_chip_id_returns_error() {
        futures::executor::block_on(async {
            let mut mock = Mock::new(&[T::write_read(ADDR, vec![REG_CHIP_ID], vec![0x00])]);
            let cfg = Bme280Config {
                i2c_addr: ADDR,
                ..Bme280Config::default()
            };
            let mut driver = Bme280::new(&cfg);
            let result = driver.init(&mut mock).await;
            assert!(matches!(result, Err(Bme280Error::InvalidId(0x00))));
            mock.done();
        });
    }

    #[test]
    fn custom_oversampling_and_filter_are_written_to_registers() {
        futures::executor::block_on(async {
            // osrs_t=X4 (3), osrs_p=X2 (2), mode=Normal → ctrl_meas = (3<<5)|(2<<2)|3 = 0x6B
            let ctrl_meas =
                ((Oversampling::X4 as u8) << 5) | ((Oversampling::X2 as u8) << 2) | MODE_NORMAL;
            // osrs_h=X8 → ctrl_hum = 4
            let ctrl_hum = Oversampling::X8 as u8;
            // t_sb=125ms (2), filter=X4 (2) → config = (2<<5)|(2<<2) = 0x48
            let config_reg = ((Standby::Ms125 as u8) << 5) | ((Filter::X4 as u8) << 2);

            let mut txns = vec![
                T::write_read(ADDR, vec![REG_CHIP_ID], vec![CHIP_ID]),
                T::write_read(ADDR, vec![REG_CALIB_TP], cal_tp_bytes()),
                T::write_read(ADDR, vec![REG_CALIB_H1], vec![CAL_H1]),
                T::write_read(ADDR, vec![REG_CALIB_H2], CAL_H2_6.to_vec()),
                T::write(ADDR, vec![REG_CTRL_HUM, ctrl_hum]),
                T::write(ADDR, vec![REG_CONFIG, config_reg]),
                T::write(ADDR, vec![REG_CTRL_MEAS, ctrl_meas]),
            ];
            let mut mock = Mock::new(&{
                txns.push(T::write_read(ADDR, vec![REG_PRESS_MSB], raw_sample_bytes()));
                txns
            });

            let cfg = Bme280Config {
                i2c_addr: ADDR,
                osrs_t: Oversampling::X4,
                osrs_p: Oversampling::X2,
                osrs_h: Oversampling::X8,
                filter: Filter::X4,
                t_sb: Standby::Ms125,
            };
            let mut driver = Bme280::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let _ = driver.sample(&mut mock).await.unwrap();
            mock.done();
        });
    }

    #[test]
    fn reinit_recovers_after_first_bring_up() {
        // The generated source task re-runs init() to recover a degraded sensor.
        // Two full bring-up sequences back-to-back on the same instance must both
        // succeed and leave the driver able to sample.
        futures::executor::block_on(async {
            let mut txns = init_transactions();
            txns.extend(init_transactions());
            txns.push(T::write_read(ADDR, vec![REG_PRESS_MSB], raw_sample_bytes()));
            let mut mock = Mock::new(&txns);

            let cfg = Bme280Config {
                i2c_addr: ADDR,
                ..Bme280Config::default()
            };
            let mut driver = Bme280::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            driver.init(&mut mock).await.unwrap(); // recovery re-init
            let readings = driver.sample(&mut mock).await.unwrap();
            assert!(readings.temperature > 20.0 && readings.temperature < 30.0);
            mock.done();
        });
    }
}
