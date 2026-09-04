//! Texas Instruments ADS1115 16-bit 4-channel ADC driver.
//!
//! Reads all four single-ended channels (AIN0–AIN3) of the ADS1115 ADC in
//! single-shot mode and returns voltages in volts. Communicates over I2C
//! using the ADS1115's address-selectable interface (`0x48`–`0x4B`
//! depending on the ADDR pin).
//!
//! # Configuration
//!
//! [`Ads1115Config`] exposes two datasheet knobs that change the readings:
//!
//! - **Full-scale range** (`fs_mv`) — see [`FullScale`]. Sets the PGA
//!   full-scale range and the per-count LSB. Lower full scale → higher
//!   resolution at the cost of clipping at large inputs.
//! - **Data rate** (`data_rate`) — see [`DataRate`]. Conversions per second;
//!   higher rates are faster but noisier.
//!
//! Defaults: ±2048 mV FSR (matches the typical `single-ended 0–3.3 V`
//! supply range with headroom), 128 SPS.
//!
//! # Timing
//!
//! Each [`Ads1115::sample`] sequentially performs four single-shot
//! conversions and waits (by polling the OS status bit) for each to
//! complete. Total sample time is roughly `4 / data_rate_sps`. At very low
//! data rates (≤ 16 SPS) the internal poll budget (`MAX_OS_POLLS`) may not
//! be sufficient and the driver will return `ConversionTimeout`.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

const REG_CONVERSION: u8 = 0x00;
const REG_CONFIG: u8 = 0x01;

// Config register bits
const CFG_OS_START: u16 = 0x8000; // Start single-shot conversion
const CFG_MODE_SINGLE: u16 = 0x0100; // Single-shot mode
const CFG_COMP_DISABLE: u16 = 0x0003; // Disable comparator

const MUX: [u16; 4] = [0x4000, 0x5000, 0x6000, 0x7000]; // AIN0..AIN3 vs GND

// At 128 SPS a conversion takes ~8 ms; 64 polls gives >500 ms of margin
// assuming each I2C poll takes >8 ms. At slower data rates (8/16/32 SPS)
// this may be insufficient and the driver will surface
// `Ads1115Error::ConversionTimeout`.
const MAX_OS_POLLS: u32 = 64;

/// PGA full-scale range.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullScale {
    /// ±256 mV — highest resolution, smallest input range.
    Mv256 = 0,
    /// ±512 mV.
    Mv512 = 1,
    /// ±1024 mV.
    Mv1024 = 2,
    /// ±2048 mV (default — typical for 3.3 V single-ended use).
    Mv2048 = 3,
    /// ±4096 mV.
    Mv4096 = 4,
    /// ±6144 mV — widest range; clips to AVDD on most boards.
    Mv6144 = 5,
}

/// Conversion data rate in samples per second.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataRate {
    /// 8 samples/sec — slowest, lowest noise.
    Sps8 = 0,
    /// 16 samples/sec.
    Sps16 = 1,
    /// 32 samples/sec.
    Sps32 = 2,
    /// 64 samples/sec.
    Sps64 = 3,
    /// 128 samples/sec (default).
    Sps128 = 4,
    /// 250 samples/sec.
    Sps250 = 5,
    /// 475 samples/sec.
    Sps475 = 6,
    /// 860 samples/sec — fastest.
    Sps860 = 7,
}

/// Driver configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ads1115Config {
    /// I2C address. Selectable by the ADDR pin: GND=`0x48`, VCC=`0x49`,
    /// SDA=`0x4A`, SCL=`0x4B`.
    pub i2c_addr: u8,
    /// PGA full-scale range.
    pub fs_mv: FullScale,
    /// Conversion data rate.
    pub data_rate: DataRate,
}

impl Default for Ads1115Config {
    /// Address `0x48`, ±2048 mV FSR, 128 SPS.
    fn default() -> Self {
        Self {
            i2c_addr: 0x48,
            fs_mv: FullScale::Mv2048,
            data_rate: DataRate::Sps128,
        }
    }
}

/// One full set of four single-ended channel readings in volts.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ads1115Readings {
    /// AIN0 vs GND, in volts.
    pub ain0: f32,
    /// AIN1 vs GND, in volts.
    pub ain1: f32,
    /// AIN2 vs GND, in volts.
    pub ain2: f32,
    /// AIN3 vs GND, in volts.
    pub ain3: f32,
}

/// Errors returned by the ADS1115 driver.
#[non_exhaustive]
#[derive(Debug)]
pub enum Ads1115Error<E: core::fmt::Debug> {
    /// Underlying I2C bus error.
    Bus(E),
    /// OS bit did not go high within `MAX_OS_POLLS` polls — likely due to a
    /// very low data rate or a stuck conversion. Retry or raise the data rate.
    ConversionTimeout,
}

impl<E: core::fmt::Debug> From<E> for Ads1115Error<E> {
    fn from(e: E) -> Self {
        Self::Bus(e)
    }
}

/// ADS1115 driver instance.
pub struct Ads1115 {
    addr: u8,
    pga_bits: u16,
    dr_bits: u16,
    lsb_mv: f32,
}

impl Ads1115 {
    /// Construct a driver instance without touching the bus.
    ///
    /// Pre-computes the PGA + data-rate config bits and per-count LSB for the
    /// configured full-scale range. The device is **not** probed until
    /// [`Ads1115::init`] is called, which must happen before [`Ads1115::sample`].
    #[must_use]
    pub fn new(cfg: &Ads1115Config) -> Self {
        let (pga_bits, lsb_mv) = pga(cfg.fs_mv);
        Self {
            addr: cfg.i2c_addr,
            pga_bits,
            dr_bits: u16::from(cfg.data_rate as u8) << 5,
            lsb_mv,
        }
    }

    /// (Re-)initialise the sensor: verify the device is present on the bus.
    ///
    /// Safe to call repeatedly — the generated source task re-runs `init` to
    /// recover a sensor that started failing.
    ///
    /// # Errors
    ///
    /// [`Ads1115Error::Bus`] on I2C transaction failure.
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), Ads1115Error<I::Error>> {
        // Probe: attempt a no-op config read to verify the device is present
        let mut buf = [0u8; 2];
        bus.write_read(self.addr, &[REG_CONFIG], &mut buf).await?;
        log::info!("[ads1115] init OK at 0x{:02X}", self.addr);
        Ok(())
    }

    /// Convert all four single-ended channels and return them in volts.
    ///
    /// Sequential single-shot conversions — total time roughly
    /// `4 / data_rate_sps`. Returns [`Ads1115Error::ConversionTimeout`] if
    /// any channel's OS bit does not go high within the poll budget.
    // ch comes from enumerate() over a 4-element array (0..3), so ch as u16 never truncates.
    #[allow(clippy::cast_possible_truncation)]
    pub async fn sample<I: I2c>(
        &mut self,
        bus: &mut I,
    ) -> Result<Ads1115Readings, Ads1115Error<I::Error>> {
        let mut channels = [0f32; 4];
        for (ch, out) in channels.iter_mut().enumerate() {
            *out = self.read_channel(bus, ch as u16).await?;
        }
        Ok(Ads1115Readings {
            ain0: channels[0],
            ain1: channels[1],
            ain2: channels[2],
            ain3: channels[3],
        })
    }

    /// Trigger one single-shot conversion on `ch` and poll the OS bit until
    /// the conversion completes (or `MAX_OS_POLLS` is reached).
    // ch is 0..3, passed from sample()'s enumerate() over a 4-element array.
    #[allow(clippy::cast_possible_truncation)]
    async fn read_channel<I: I2c>(
        &self,
        bus: &mut I,
        ch: u16,
    ) -> Result<f32, Ads1115Error<I::Error>> {
        let cfg = CFG_OS_START
            | MUX[ch as usize]
            | self.pga_bits
            | CFG_MODE_SINGLE
            | self.dr_bits
            | CFG_COMP_DISABLE;
        let cfg_bytes = cfg.to_be_bytes();
        bus.write(self.addr, &[REG_CONFIG, cfg_bytes[0], cfg_bytes[1]])
            .await?;
        // Poll OS bit (bit 15): reads 0 while conversion is in progress, 1 when done.
        let mut polls = 0u32;
        loop {
            let mut status = [0u8; 2];
            bus.write_read(self.addr, &[REG_CONFIG], &mut status)
                .await?;
            if u16::from_be_bytes(status) & 0x8000 != 0 {
                break;
            }
            polls += 1;
            if polls >= MAX_OS_POLLS {
                return Err(Ads1115Error::ConversionTimeout);
            }
        }
        let mut raw = [0u8; 2];
        bus.write_read(self.addr, &[REG_CONVERSION], &mut raw)
            .await?;
        let counts = i16::from_be_bytes(raw);
        Ok(f32::from(counts) * self.lsb_mv / 1000.0) // mV → V
    }
}

/// Returns (PGA config bits, LSB size in mV) for a full-scale range.
fn pga(fs: FullScale) -> (u16, f32) {
    match fs {
        FullScale::Mv256 => (0x0A00, 0.007_812_5),
        FullScale::Mv512 => (0x0800, 0.015_625),
        FullScale::Mv1024 => (0x0600, 0.03125),
        FullScale::Mv2048 => (0x0400, 0.0625),
        FullScale::Mv4096 => (0x0200, 0.125),
        FullScale::Mv6144 => (0x0000, 0.1875),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};

    const ADDR: u8 = 0x48;

    #[allow(clippy::cast_possible_truncation)]
    fn sample_transactions(voltages_mv: [i16; 4], dr_bits: u16) -> Vec<T> {
        let (pga_bits, lsb_mv) = pga(FullScale::Mv2048);
        let mut txns = Vec::new();
        for (ch, &mv) in voltages_mv.iter().enumerate() {
            let counts = (f32::from(mv) / lsb_mv) as i16;
            let cfg =
                CFG_OS_START | MUX[ch] | pga_bits | CFG_MODE_SINGLE | dr_bits | CFG_COMP_DISABLE;
            let cb = cfg.to_be_bytes();
            txns.push(T::write(ADDR, vec![REG_CONFIG, cb[0], cb[1]]));
            // OS bit poll: return ready (bit 15 set) on first poll.
            txns.push(T::write_read(ADDR, vec![REG_CONFIG], vec![0x80, 0x00]));
            txns.push(T::write_read(
                ADDR,
                vec![REG_CONVERSION],
                counts.to_be_bytes().to_vec(),
            ));
        }
        txns
    }

    #[test]
    fn init_and_sample_defaults() {
        futures::executor::block_on(async {
            let dr_bits = u16::from(DataRate::Sps128 as u8) << 5;
            let mut txns = vec![T::write_read(ADDR, vec![REG_CONFIG], vec![0x85, 0x83])];
            txns.extend(sample_transactions([1000, 2000, 500, 1500], dr_bits));
            let mut mock = Mock::new(&txns);

            let cfg = Ads1115Config {
                i2c_addr: ADDR,
                ..Ads1115Config::default()
            };
            let mut driver = Ads1115::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();

            assert!((r.ain0 - 1.0).abs() < 0.01, "ain0={}", r.ain0);
            assert!((r.ain1 - 2.0).abs() < 0.01, "ain1={}", r.ain1);
            assert!((r.ain2 - 0.5).abs() < 0.01, "ain2={}", r.ain2);
            assert!((r.ain3 - 1.5).abs() < 0.01, "ain3={}", r.ain3);

            mock.done();
        });
    }

    #[test]
    fn custom_data_rate_propagates_to_config_register() {
        futures::executor::block_on(async {
            let dr_bits = u16::from(DataRate::Sps860 as u8) << 5;
            let mut txns = vec![T::write_read(ADDR, vec![REG_CONFIG], vec![0x85, 0x83])];
            txns.extend(sample_transactions([0, 0, 0, 0], dr_bits));
            let mut mock = Mock::new(&txns);

            let cfg = Ads1115Config {
                i2c_addr: ADDR,
                data_rate: DataRate::Sps860,
                ..Ads1115Config::default()
            };
            let mut driver = Ads1115::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let _ = driver.sample(&mut mock).await.unwrap();
            mock.done();
        });
    }

    #[test]
    fn high_resolution_full_scale_uses_smallest_lsb() {
        futures::executor::block_on(async {
            let (pga_bits, lsb_mv) = pga(FullScale::Mv256);
            let dr_bits = u16::from(DataRate::Sps128 as u8) << 5;
            // 100 mV input → counts = 100 / 0.0078125 ≈ 12800
            let counts: i16 = (100.0_f32 / lsb_mv) as i16;
            let cfg_word =
                CFG_OS_START | MUX[0] | pga_bits | CFG_MODE_SINGLE | dr_bits | CFG_COMP_DISABLE;
            let cb = cfg_word.to_be_bytes();
            let mut txns = vec![T::write_read(ADDR, vec![REG_CONFIG], vec![0x85, 0x83])];
            // only channel 0 — build manually to use Mv256 pga_bits
            txns.push(T::write(ADDR, vec![REG_CONFIG, cb[0], cb[1]]));
            txns.push(T::write_read(ADDR, vec![REG_CONFIG], vec![0x80, 0x00]));
            txns.push(T::write_read(
                ADDR,
                vec![REG_CONVERSION],
                counts.to_be_bytes().to_vec(),
            ));
            // channels 1-3 zero
            let (pga_bits2, _) = pga(FullScale::Mv256);
            for ch in 1..4usize {
                let cw = CFG_OS_START
                    | MUX[ch]
                    | pga_bits2
                    | CFG_MODE_SINGLE
                    | dr_bits
                    | CFG_COMP_DISABLE;
                let cb2 = cw.to_be_bytes();
                txns.push(T::write(ADDR, vec![REG_CONFIG, cb2[0], cb2[1]]));
                txns.push(T::write_read(ADDR, vec![REG_CONFIG], vec![0x80, 0x00]));
                txns.push(T::write_read(
                    ADDR,
                    vec![REG_CONVERSION],
                    0i16.to_be_bytes().to_vec(),
                ));
            }
            let mut mock = Mock::new(&txns);

            let cfg = Ads1115Config {
                i2c_addr: ADDR,
                fs_mv: FullScale::Mv256,
                ..Ads1115Config::default()
            };
            let mut driver = Ads1115::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();
            assert!((r.ain0 - 0.1).abs() < 0.001, "ain0={}", r.ain0);
            mock.done();
        });
    }

    #[test]
    fn reinit_recovers_after_first_bring_up() {
        // The generated source task re-runs init() to recover a degraded sensor.
        futures::executor::block_on(async {
            let dr_bits = u16::from(DataRate::Sps128 as u8) << 5;
            let mut txns = vec![
                T::write_read(ADDR, vec![REG_CONFIG], vec![0x85, 0x83]), // first probe
                T::write_read(ADDR, vec![REG_CONFIG], vec![0x85, 0x83]), // recovery re-probe
            ];
            txns.extend(sample_transactions([1000, 0, 0, 0], dr_bits));
            let mut mock = Mock::new(&txns);

            let cfg = Ads1115Config {
                i2c_addr: ADDR,
                ..Ads1115Config::default()
            };
            let mut driver = Ads1115::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            driver.init(&mut mock).await.unwrap(); // recovery re-init
            let r = driver.sample(&mut mock).await.unwrap();
            assert!((r.ain0 - 1.0).abs() < 0.01, "ain0={}", r.ain0);
            mock.done();
        });
    }
}
