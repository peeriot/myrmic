//! `ScioSense` CCS811 indoor air-quality sensor driver.
//!
//! Reports equivalent CO2 (eCO2, in ppm) and total volatile organic
//! compounds (TVOC, in ppb) over I2C, with optional data-ready interrupt on
//! the NINT pin.
//!
//! # Configuration
//!
//! [`Ccs811Config`] exposes the I2C address and the measurement mode
//! (`meas_mode`) — see [`MeasMode`]. The mode controls how often the sensor's
//! internal algorithm produces a fresh sample:
//!
//! - [`MeasMode::Every1s`] (default) — one reading per second.
//! - [`MeasMode::Every10s`] — one reading per 10 s (lower power, longer warm-up).
//! - [`MeasMode::Every60s`] — one reading per minute.
//!
//! Idle mode (no sampling) and the 250 ms raw-data-only mode are not exposed
//! through this driver, since they don't fit the Signal Layer's "init then poll"
//! workflow.
//!
//! # Pins
//!
//! The optional NINT (data-ready interrupt) line is supported via
//! [`Ccs811::init_with_pins`] and [`Ccs811Pins`]. When NINT is wired the
//! driver enables hardware interrupt mode and waits for the line to assert
//! before reading; otherwise it polls the STATUS register. Both paths
//! defensively check `STATUS_DATA_READY` and the algorithm error bit before
//! returning a reading.
//!
//! # Timing
//!
//! `init()` software-resets the device (100 ms wait per datasheet) and may
//! start the firmware (20 ms wait) — handled with `embassy_time` under
//! `#[cfg(not(test))]`. Host tests skip the waits since they use
//! deterministic mocks.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

const REG_STATUS: u8 = 0x00;
const REG_MEAS_MODE: u8 = 0x01;
const REG_ALG_RESULT: u8 = 0x02;
const REG_HW_ID: u8 = 0x20;
const REG_APP_START: u8 = 0xF4;
const REG_SW_RESET: u8 = 0xFF;

const HW_ID: u8 = 0x81;
const SW_RESET_SEQ: [u8; 5] = [REG_SW_RESET, 0x11, 0xE5, 0x72, 0x8A];

const STATUS_FW_MODE: u8 = 0x80;
const STATUS_APP_VALID: u8 = 0x10;
const STATUS_DATA_READY: u8 = 0x08;
const STATUS_ERROR: u8 = 0x01;

/// Measurement mode — controls how often the CCS811 algorithm produces a new sample.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasMode {
    /// One reading per second (default).
    Every1s = 1,
    /// One reading per 10 seconds (lower power).
    Every10s = 2,
    /// One reading per minute (lowest power).
    Every60s = 3,
}

const INT_ENABLE_BIT: u8 = 0x08;

fn meas_mode_register(meas_mode: MeasMode, irq: bool) -> u8 {
    // DRIVE_MODE occupies bits 6:4 (so `meas_mode << 4`); INT_DATARDY is bit 3.
    let mut reg = (meas_mode as u8) << 4;
    if irq {
        reg |= INT_ENABLE_BIT;
    }
    reg
}

/// Driver configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ccs811Config {
    /// I2C address. Factory default `0x5A` (ADDR low); `0x5B` when ADDR is
    /// pulled high.
    pub i2c_addr: u8,
    /// Measurement mode (how often the algorithm produces a fresh sample).
    pub meas_mode: MeasMode,
}

impl Default for Ccs811Config {
    fn default() -> Self {
        Self {
            i2c_addr: 0x5A,
            meas_mode: MeasMode::Every1s,
        }
    }
}

/// One air-quality reading.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ccs811Readings {
    /// Equivalent CO2 in parts per million.
    pub eco2: f32,
    /// Total volatile organic compounds in parts per billion.
    pub tvoc: f32,
}

/// Errors returned by the CCS811 driver.
#[non_exhaustive]
#[derive(Debug)]
pub enum Ccs811Error<E: core::fmt::Debug> {
    /// Underlying I2C bus error.
    Bus(E),
    /// `HW_ID` register did not return `0x81`.
    InvalidId(u8),
    /// `STATUS` register reported `APP_VALID=0` — sensor has no valid firmware.
    NoValidFirmware,
    /// Algorithm result reported the `STATUS_ERROR` bit.
    SensorError,
    /// `sample()` was called but `STATUS_DATA_READY` was not asserted.
    NotReady,
    /// NINT did not assert within the interrupt timeout (5 s).
    Timeout,
    /// Underlying GPIO/Wait error on the NINT pin.
    Pin,
}

impl<E: core::fmt::Debug> From<E> for Ccs811Error<E> {
    fn from(e: E) -> Self {
        Self::Bus(e)
    }
}

/// Marker type used when no NINT pin is wired. Implements `Wait` as a
/// never-resolving stub so a single generic `sample()` impl can serve both
/// the with-pin and no-pin cases without code duplication.
pub struct NoPin;

impl embedded_hal::digital::ErrorType for NoPin {
    type Error = core::convert::Infallible;
}

impl embedded_hal_async::digital::Wait for NoPin {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        core::future::pending::<()>().await;
        unreachable!()
    }
    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        core::future::pending::<()>().await;
        unreachable!()
    }
    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        core::future::pending::<()>().await;
        unreachable!()
    }
    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        core::future::pending::<()>().await;
        unreachable!()
    }
    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        core::future::pending::<()>().await;
        unreachable!()
    }
}

/// Optional GPIO pins for the CCS811.
///
/// The driver picks up whichever fields are `Some(..)`. Currently only NINT
/// (the data-ready interrupt input) is supported. Codegen emits struct
/// literals here, so this stays exhaustive — extending it is a breaking
/// change tracked through the descriptor's `optional_pins` list.
pub struct Ccs811Pins<P> {
    /// NINT — active-low data-ready interrupt. When wired, the driver
    /// enables hardware interrupt mode and uses `wait_for_low()` instead of
    /// polling STATUS in [`Ccs811::sample`].
    pub nint: Option<P>,
}

impl<P> Default for Ccs811Pins<P> {
    fn default() -> Self {
        Self { nint: None }
    }
}

/// CCS811 driver instance.
pub struct Ccs811<P = NoPin> {
    cfg: Ccs811Config,
    nint: Option<P>,
}

impl Ccs811<NoPin> {
    /// Construct a polling-mode driver instance (no NINT) without touching the
    /// bus. Equivalent to [`Ccs811::new_with_pins`] with `Ccs811Pins { nint: None }`.
    ///
    /// The sensor is **not** initialised yet — call [`Ccs811::init`] before
    /// [`Ccs811::sample`].
    #[must_use]
    pub fn new(cfg: &Ccs811Config) -> Self {
        Self {
            cfg: *cfg,
            nint: None,
        }
    }
}

impl<P: embedded_hal_async::digital::Wait> Ccs811<P> {
    /// Construct a driver instance with the given pin set, without touching the
    /// bus. If `pins.nint` is `Some`, [`Ccs811::init`] enables hardware
    /// interrupt mode (the sensor pulls NINT low when a new sample is ready);
    /// if `None`, polling mode is used and `Ccs811<P>` behaves identically to
    /// `Ccs811<NoPin>`.
    ///
    /// The sensor is **not** initialised yet — call [`Ccs811::init`] before
    /// [`Ccs811::sample`].
    #[must_use]
    pub fn new_with_pins(cfg: &Ccs811Config, pins: Ccs811Pins<P>) -> Self {
        Self {
            cfg: *cfg,
            nint: pins.nint,
        }
    }

    /// (Re-)initialise the sensor: SW reset, verify `HW_ID`, transition from
    /// boot to app mode if needed, then write `MEAS_MODE` (with the interrupt
    /// bit set when NINT is wired).
    ///
    /// Safe to call repeatedly — the generated source task re-runs `init` to
    /// recover a sensor that started failing. The owned NINT pin is retained
    /// across re-inits.
    ///
    /// # Errors
    ///
    /// - [`Ccs811Error::Bus`] on any I2C transaction failure.
    /// - [`Ccs811Error::InvalidId`] if the `HW_ID` register does not read `0x81`.
    /// - [`Ccs811Error::NoValidFirmware`] if the sensor has no valid application.
    pub async fn init<I: I2c>(&mut self, bus: &mut I) -> Result<(), Ccs811Error<I::Error>> {
        let irq = self.nint.is_some();
        init_sensor(
            bus,
            self.cfg.i2c_addr,
            meas_mode_register(self.cfg.meas_mode, irq),
        )
        .await
    }

    /// Read one eCO2/TVOC sample. When NINT is wired, blocks (up to 5 s)
    /// waiting for the line to assert; otherwise polls STATUS once and
    /// returns [`Ccs811Error::NotReady`] if no fresh data is available.
    ///
    /// Both paths defensively check `STATUS_ERROR` and `STATUS_DATA_READY`.
    ///
    /// # Errors
    ///
    /// - [`Ccs811Error::Bus`] on I2C transaction failure.
    /// - [`Ccs811Error::Pin`] / `Timeout` on NINT failure or timeout.
    /// - [`Ccs811Error::SensorError`] if the algorithm error bit is set.
    /// - [`Ccs811Error::NotReady`] (polling path only) if data isn't ready
    ///   yet — caller should retry after the configured `meas_mode` cadence.
    pub async fn sample<I: I2c>(
        &mut self,
        bus: &mut I,
    ) -> Result<Ccs811Readings, Ccs811Error<I::Error>> {
        if let Some(nint) = self.nint.as_mut() {
            #[cfg(not(test))]
            embassy_time::with_timeout(embassy_time::Duration::from_secs(5), nint.wait_for_low())
                .await
                .map_err(|_| Ccs811Error::Timeout)?
                .map_err(|_| Ccs811Error::Pin)?;
            #[cfg(test)]
            nint.wait_for_low().await.map_err(|_| Ccs811Error::Pin)?;
        }
        read_alg_result(bus, self.cfg.i2c_addr, self.nint.is_some()).await
    }
}

/// Read the 8-byte `ALG_RESULT` register and decode eCO2 + TVOC.
/// `interrupt_driven`: when true, the NINT line already implies data is
/// ready, so `STATUS_DATA_READY` is treated as defensive (still checked —
/// missing it returns [`Ccs811Error::NotReady`] rather than silently
/// returning stale data).
async fn read_alg_result<I: I2c>(
    bus: &mut I,
    addr: u8,
    _interrupt_driven: bool,
) -> Result<Ccs811Readings, Ccs811Error<I::Error>> {
    let mut result = [0u8; 8];
    bus.write_read(addr, &[REG_ALG_RESULT], &mut result).await?;

    let status = result[4];
    if status & STATUS_ERROR != 0 {
        return Err(Ccs811Error::SensorError);
    }
    if status & STATUS_DATA_READY == 0 {
        return Err(Ccs811Error::NotReady);
    }

    let eco2 = f32::from(u16::from_be_bytes([result[0], result[1]]));
    let tvoc = f32::from(u16::from_be_bytes([result[2], result[3]]));

    log::debug!("[ccs811] eCO2={eco2}ppm TVOC={tvoc}ppb");
    Ok(Ccs811Readings { eco2, tvoc })
}

/// Shared init sequence: SW reset, verify `HW_ID`, transition from boot to
/// app mode if needed, then write `MEAS_MODE`.
async fn init_sensor<I: I2c>(
    bus: &mut I,
    addr: u8,
    meas_mode_reg: u8,
) -> Result<(), Ccs811Error<I::Error>> {
    // SW reset — CCS811 requires 100 ms before any further interaction.
    bus.write(addr, &SW_RESET_SEQ).await?;
    #[cfg(not(test))]
    embassy_time::Timer::after_millis(100).await;

    let mut hw_id = [0u8; 1];
    bus.write_read(addr, &[REG_HW_ID], &mut hw_id).await?;
    if hw_id[0] != HW_ID {
        return Err(Ccs811Error::InvalidId(hw_id[0]));
    }

    let mut status = [0u8; 1];
    bus.write_read(addr, &[REG_STATUS], &mut status).await?;
    if status[0] & STATUS_APP_VALID == 0 {
        return Err(Ccs811Error::NoValidFirmware);
    }
    if status[0] & STATUS_FW_MODE == 0 {
        // Boot mode — start the application and wait for the boot→app transition.
        bus.write(addr, &[REG_APP_START]).await?;
        #[cfg(not(test))]
        embassy_time::Timer::after_millis(20).await;
    }

    bus.write(addr, &[REG_MEAS_MODE, meas_mode_reg]).await?;

    log::info!("[ccs811] init OK at 0x{addr:02X}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction as T};

    const ADDR: u8 = 0x5A;

    fn init_transactions_app_mode(meas_mode_reg: u8) -> Vec<T> {
        vec![
            T::write(ADDR, SW_RESET_SEQ.to_vec()),
            T::write_read(ADDR, vec![REG_HW_ID], vec![HW_ID]),
            T::write_read(
                ADDR,
                vec![REG_STATUS],
                vec![STATUS_FW_MODE | STATUS_APP_VALID],
            ),
            T::write(ADDR, vec![REG_MEAS_MODE, meas_mode_reg]),
        ]
    }

    #[test]
    fn init_and_sample_defaults() {
        futures::executor::block_on(async {
            let eco2_bytes = 700u16.to_be_bytes();
            let tvoc_bytes = 200u16.to_be_bytes();
            let alg_result = vec![
                eco2_bytes[0],
                eco2_bytes[1],
                tvoc_bytes[0],
                tvoc_bytes[1],
                STATUS_DATA_READY,
                0x00,
                0x00,
                0x00,
            ];

            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut txns = init_transactions_app_mode(polling_mode);
            txns.push(T::write_read(ADDR, vec![REG_ALG_RESULT], alg_result));
            let mut mock = Mock::new(&txns);

            let cfg = Ccs811Config {
                i2c_addr: ADDR,
                ..Ccs811Config::default()
            };
            let mut driver = Ccs811::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();

            assert!((r.eco2 - 700.0).abs() < 1.0, "eco2={}", r.eco2);
            assert!((r.tvoc - 200.0).abs() < 1.0, "tvoc={}", r.tvoc);

            mock.done();
        });
    }

    #[test]
    fn boot_mode_triggers_app_start() {
        futures::executor::block_on(async {
            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut mock = Mock::new(&[
                T::write(ADDR, SW_RESET_SEQ.to_vec()),
                T::write_read(ADDR, vec![REG_HW_ID], vec![HW_ID]),
                // Status: APP_VALID but not FW_MODE → boot mode
                T::write_read(ADDR, vec![REG_STATUS], vec![STATUS_APP_VALID]),
                T::write(ADDR, vec![REG_APP_START]),
                T::write(ADDR, vec![REG_MEAS_MODE, polling_mode]),
            ]);

            let cfg = Ccs811Config {
                i2c_addr: ADDR,
                ..Ccs811Config::default()
            };
            let mut driver = Ccs811::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            mock.done();
        });
    }

    #[test]
    fn not_ready_returns_error() {
        futures::executor::block_on(async {
            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut txns = init_transactions_app_mode(polling_mode);
            txns.push(T::write_read(
                ADDR,
                vec![REG_ALG_RESULT],
                vec![0, 0, 0, 0, STATUS_FW_MODE, 0, 0, 0],
            ));
            let mut mock = Mock::new(&txns);

            let cfg = Ccs811Config {
                i2c_addr: ADDR,
                ..Ccs811Config::default()
            };
            let mut driver = Ccs811::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            let result = driver.sample(&mut mock).await;
            assert!(matches!(result, Err(Ccs811Error::NotReady)));

            mock.done();
        });
    }

    struct ImmediateLow;

    impl embedded_hal::digital::ErrorType for ImmediateLow {
        type Error = core::convert::Infallible;
    }

    impl embedded_hal_async::digital::Wait for ImmediateLow {
        async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn init_with_pins_none_falls_back_to_polling() {
        futures::executor::block_on(async {
            let eco2_bytes = 600u16.to_be_bytes();
            let tvoc_bytes = 250u16.to_be_bytes();
            let alg_result = vec![
                eco2_bytes[0],
                eco2_bytes[1],
                tvoc_bytes[0],
                tvoc_bytes[1],
                STATUS_DATA_READY,
                0x00,
                0x00,
                0x00,
            ];

            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut txns = init_transactions_app_mode(polling_mode);
            txns.push(T::write_read(ADDR, vec![REG_ALG_RESULT], alg_result));
            let mut mock = Mock::new(&txns);

            let cfg = Ccs811Config {
                i2c_addr: ADDR,
                ..Ccs811Config::default()
            };
            let pins: Ccs811Pins<ImmediateLow> = Ccs811Pins { nint: None };
            let mut driver = Ccs811::new_with_pins(&cfg, pins);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();

            assert!((r.eco2 - 600.0).abs() < 1.0, "eco2={}", r.eco2);
            assert!((r.tvoc - 250.0).abs() < 1.0, "tvoc={}", r.tvoc);

            mock.done();
        });
    }

    #[test]
    fn interrupt_driven_init_and_sample() {
        futures::executor::block_on(async {
            let eco2_bytes = 800u16.to_be_bytes();
            let tvoc_bytes = 150u16.to_be_bytes();
            let alg_result = vec![
                eco2_bytes[0],
                eco2_bytes[1],
                tvoc_bytes[0],
                tvoc_bytes[1],
                STATUS_DATA_READY,
                0x00,
                0x00,
                0x00,
            ];

            let irq_mode = meas_mode_register(MeasMode::Every1s, true);
            let mut txns = init_transactions_app_mode(irq_mode);
            txns.push(T::write_read(ADDR, vec![REG_ALG_RESULT], alg_result));

            let mut mock = Mock::new(&txns);
            let cfg = Ccs811Config {
                i2c_addr: ADDR,
                ..Ccs811Config::default()
            };
            let pins = Ccs811Pins {
                nint: Some(ImmediateLow),
            };
            let mut driver = Ccs811::new_with_pins(&cfg, pins);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();

            assert!((r.eco2 - 800.0).abs() < 1.0, "eco2={}", r.eco2);
            assert!((r.tvoc - 150.0).abs() < 1.0, "tvoc={}", r.tvoc);

            mock.done();
        });
    }

    #[test]
    fn interrupt_path_still_checks_data_ready() {
        // Interrupt fires (NINT low) but somehow STATUS_DATA_READY isn't
        // set — defensive check must still return NotReady, not stale data.
        futures::executor::block_on(async {
            let alg_result = vec![
                0,
                0,
                0,
                0,
                STATUS_FW_MODE, // no DATA_READY bit
                0,
                0,
                0,
            ];

            let irq_mode = meas_mode_register(MeasMode::Every1s, true);
            let mut txns = init_transactions_app_mode(irq_mode);
            txns.push(T::write_read(ADDR, vec![REG_ALG_RESULT], alg_result));

            let mut mock = Mock::new(&txns);
            let cfg = Ccs811Config {
                i2c_addr: ADDR,
                ..Ccs811Config::default()
            };
            let pins = Ccs811Pins {
                nint: Some(ImmediateLow),
            };
            let mut driver = Ccs811::new_with_pins(&cfg, pins);
            driver.init(&mut mock).await.unwrap();
            let result = driver.sample(&mut mock).await;
            assert!(matches!(result, Err(Ccs811Error::NotReady)));
            mock.done();
        });
    }

    #[test]
    fn reinit_recovers_after_first_bring_up() {
        // The generated source task re-runs init() to recover a degraded sensor.
        // The owned NINT pin (here None) is retained across re-inits.
        futures::executor::block_on(async {
            let eco2_bytes = 700u16.to_be_bytes();
            let tvoc_bytes = 200u16.to_be_bytes();
            let alg_result = vec![
                eco2_bytes[0],
                eco2_bytes[1],
                tvoc_bytes[0],
                tvoc_bytes[1],
                STATUS_DATA_READY,
                0x00,
                0x00,
                0x00,
            ];

            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut txns = init_transactions_app_mode(polling_mode);
            txns.extend(init_transactions_app_mode(polling_mode)); // recovery re-init
            txns.push(T::write_read(ADDR, vec![REG_ALG_RESULT], alg_result));
            let mut mock = Mock::new(&txns);

            let cfg = Ccs811Config {
                i2c_addr: ADDR,
                ..Ccs811Config::default()
            };
            let mut driver = Ccs811::new(&cfg);
            driver.init(&mut mock).await.unwrap();
            driver.init(&mut mock).await.unwrap(); // recovery re-init
            let r = driver.sample(&mut mock).await.unwrap();
            assert!((r.eco2 - 700.0).abs() < 1.0, "eco2={}", r.eco2);
            mock.done();
        });
    }
}
