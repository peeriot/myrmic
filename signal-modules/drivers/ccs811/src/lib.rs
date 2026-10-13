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
//! [`Ccs811::new_with_pins`] and [`Ccs811Pins`]. When NINT is wired the
//! driver enables hardware interrupt mode and waits for the line to assert
//! before checking STATUS; otherwise it polls the STATUS register until
//! `DATA_READY` is set. Both paths check the algorithm error bit before
//! returning a reading.
//!
//! # Timing
//!
//! `init()` software-resets the device (100 ms wait per datasheet) and may
//! start the firmware (20 ms wait). The sensor then needs one full
//! `meas_mode` period before its first result exists, so [`Ccs811::sample`]
//! waits for `DATA_READY` — up to the period plus a 50 % margin — before it
//! reads `ALG_RESULT_DATA`. All waits use `embassy_time` under
//! `#[cfg(not(test))]`; host tests skip them since they use deterministic
//! mocks.

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

impl MeasMode {
    /// Nominal interval between two algorithm results, in milliseconds.
    #[must_use]
    pub const fn period_ms(self) -> u64 {
        match self {
            Self::Every1s => 1_000,
            Self::Every10s => 10_000,
            Self::Every60s => 60_000,
        }
    }
}

const INT_ENABLE_BIT: u8 = 0x08;

/// Interval between STATUS reads while waiting for `DATA_READY`.
const DATA_READY_POLL_MS: u64 = 100;

fn meas_mode_register(meas_mode: MeasMode, irq: bool) -> u8 {
    // DRIVE_MODE occupies bits 6:4 (so `meas_mode << 4`); INT_DATARDY is bit 3.
    let mut reg = (meas_mode as u8) << 4;
    if irq {
        reg |= INT_ENABLE_BIT;
    }
    reg
}

/// Longest wait for a fresh sample: one `meas_mode` period plus a 50 % margin
/// for the sensor's clock tolerance and the latency of its first result.
fn data_ready_timeout_ms(meas_mode: MeasMode) -> u64 {
    let period = meas_mode.period_ms();
    period + period / 2
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
    /// No fresh sample within the `meas_mode` period plus margin — NINT did
    /// not assert, or `STATUS` never reported `DATA_READY`.
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
    /// enables hardware interrupt mode and waits on `wait_for_low()` before
    /// checking STATUS in [`Ccs811::sample`], instead of polling it.
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

    /// Read one eCO2/TVOC sample, waiting for the sensor to finish its
    /// current measurement first.
    ///
    /// When NINT is wired, waits for the line to assert; otherwise polls
    /// STATUS at a fixed interval. Either wait is bounded by the `meas_mode`
    /// period plus a 50 % margin, so a sensor that produces no result within
    /// that window is reported as [`Ccs811Error::Timeout`].
    ///
    /// The first call after [`Ccs811::init`] always waits for one full
    /// period — 1 s, 10 s or 60 s — because the reset restarts the
    /// measurement cycle. That is deliberate: the generated source task has
    /// no "no data yet" outcome (any `Err` triggers a re-init and another
    /// reset), its ticker catches up on missed ticks, and nothing bounds how
    /// long one tick may take. Later calls return as soon as a result is
    /// pending, so the effective cadence is the larger of the task's sample
    /// interval and the mode period.
    ///
    /// # Errors
    ///
    /// - [`Ccs811Error::Bus`] on I2C transaction failure.
    /// - [`Ccs811Error::Pin`] if waiting on NINT fails.
    /// - [`Ccs811Error::Timeout`] if no fresh sample appears within the window.
    /// - [`Ccs811Error::SensorError`] if the algorithm error bit is set.
    pub async fn sample<I: I2c>(
        &mut self,
        bus: &mut I,
    ) -> Result<Ccs811Readings, Ccs811Error<I::Error>> {
        let timeout_ms = data_ready_timeout_ms(self.cfg.meas_mode);
        if let Some(nint) = self.nint.as_mut() {
            #[cfg(not(test))]
            embassy_time::with_timeout(
                embassy_time::Duration::from_millis(timeout_ms),
                nint.wait_for_low(),
            )
            .await
            .map_err(|_| Ccs811Error::Timeout)?
            .map_err(|_| Ccs811Error::Pin)?;
            #[cfg(test)]
            nint.wait_for_low().await.map_err(|_| Ccs811Error::Pin)?;
        }
        wait_data_ready(bus, self.cfg.i2c_addr, timeout_ms).await?;
        read_alg_result(bus, self.cfg.i2c_addr).await
    }
}

/// Poll `STATUS` until `DATA_READY` is set, sleeping `DATA_READY_POLL_MS`
/// between reads, for at most `timeout_ms`.
async fn wait_data_ready<I: I2c>(
    bus: &mut I,
    addr: u8,
    timeout_ms: u64,
) -> Result<(), Ccs811Error<I::Error>> {
    let mut polls_left = timeout_ms / DATA_READY_POLL_MS;
    let mut status = [0u8; 1];
    loop {
        bus.write_read(addr, &[REG_STATUS], &mut status).await?;
        if status[0] & STATUS_ERROR != 0 {
            return Err(Ccs811Error::SensorError);
        }
        if status[0] & STATUS_DATA_READY != 0 {
            return Ok(());
        }
        if polls_left == 0 {
            return Err(Ccs811Error::Timeout);
        }
        polls_left -= 1;
        #[cfg(not(test))]
        embassy_time::Timer::after_millis(DATA_READY_POLL_MS).await;
    }
}

/// Read the 8-byte `ALG_RESULT_DATA` register and decode eCO2 + TVOC. The
/// caller has already seen `DATA_READY`; the STATUS copy in byte 4 is only
/// re-checked for the error bit.
async fn read_alg_result<I: I2c>(
    bus: &mut I,
    addr: u8,
) -> Result<Ccs811Readings, Ccs811Error<I::Error>> {
    let mut result = [0u8; 8];
    bus.write_read(addr, &[REG_ALG_RESULT], &mut result).await?;

    if result[4] & STATUS_ERROR != 0 {
        return Err(Ccs811Error::SensorError);
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
    const ALL_MODES: [MeasMode; 3] = [MeasMode::Every1s, MeasMode::Every10s, MeasMode::Every60s];

    /// App running, no result pending yet.
    const STATUS_NO_DATA: u8 = STATUS_FW_MODE | STATUS_APP_VALID;
    /// App running with a fresh result in `ALG_RESULT_DATA`.
    const STATUS_READY: u8 = STATUS_NO_DATA | STATUS_DATA_READY;

    fn config(meas_mode: MeasMode) -> Ccs811Config {
        Ccs811Config {
            i2c_addr: ADDR,
            meas_mode,
        }
    }

    fn init_transactions_app_mode(meas_mode_reg: u8) -> Vec<T> {
        vec![
            T::write(ADDR, SW_RESET_SEQ.to_vec()),
            T::write_read(ADDR, vec![REG_HW_ID], vec![HW_ID]),
            T::write_read(ADDR, vec![REG_STATUS], vec![STATUS_NO_DATA]),
            T::write(ADDR, vec![REG_MEAS_MODE, meas_mode_reg]),
        ]
    }

    fn status_read(status: u8) -> T {
        T::write_read(ADDR, vec![REG_STATUS], vec![status])
    }

    fn alg_result_read(eco2: u16, tvoc: u16) -> T {
        let eco2 = eco2.to_be_bytes();
        let tvoc = tvoc.to_be_bytes();
        T::write_read(
            ADDR,
            vec![REG_ALG_RESULT],
            vec![
                eco2[0],
                eco2[1],
                tvoc[0],
                tvoc[1],
                STATUS_READY,
                0x00,
                0x00,
                0x00,
            ],
        )
    }

    /// STATUS reads `sample()` makes before giving up: one immediately, then
    /// one per poll interval until the timeout is spent.
    fn status_reads_until_timeout(meas_mode: MeasMode) -> u64 {
        data_ready_timeout_ms(meas_mode) / DATA_READY_POLL_MS + 1
    }

    #[test]
    fn init_and_sample_defaults() {
        futures::executor::block_on(async {
            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut txns = init_transactions_app_mode(polling_mode);
            txns.push(status_read(STATUS_READY));
            txns.push(alg_result_read(700, 200));
            let mut mock = Mock::new(&txns);

            let mut driver = Ccs811::new(&config(MeasMode::Every1s));
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

            let mut driver = Ccs811::new(&config(MeasMode::Every1s));
            driver.init(&mut mock).await.unwrap();
            mock.done();
        });
    }

    #[test]
    fn sample_waits_for_data_ready() {
        // The first STATUS reads after init report no result yet; the driver
        // must keep polling and deliver exactly one reading once DATA_READY
        // is set, with no bus traffic beyond that.
        futures::executor::block_on(async {
            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut txns = init_transactions_app_mode(polling_mode);
            txns.push(status_read(STATUS_NO_DATA));
            txns.push(status_read(STATUS_NO_DATA));
            txns.push(status_read(STATUS_READY));
            txns.push(alg_result_read(700, 200));
            let mut mock = Mock::new(&txns);

            let mut driver = Ccs811::new(&config(MeasMode::Every1s));
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();

            assert!((r.eco2 - 700.0).abs() < 1.0, "eco2={}", r.eco2);
            assert!((r.tvoc - 200.0).abs() < 1.0, "tvoc={}", r.tvoc);

            mock.done();
        });
    }

    #[test]
    fn sample_times_out_without_data_ready() {
        for meas_mode in ALL_MODES {
            futures::executor::block_on(async {
                let polling_mode = meas_mode_register(meas_mode, false);
                let mut txns = init_transactions_app_mode(polling_mode);
                for _ in 0..status_reads_until_timeout(meas_mode) {
                    txns.push(status_read(STATUS_NO_DATA));
                }
                let mut mock = Mock::new(&txns);

                let mut driver = Ccs811::new(&config(meas_mode));
                driver.init(&mut mock).await.unwrap();
                let result = driver.sample(&mut mock).await;
                assert!(
                    matches!(result, Err(Ccs811Error::Timeout)),
                    "{meas_mode:?}: {result:?}"
                );

                mock.done();
            });
        }
    }

    #[test]
    fn data_ready_timeout_exceeds_period() {
        for meas_mode in ALL_MODES {
            assert!(
                data_ready_timeout_ms(meas_mode) > meas_mode.period_ms(),
                "{meas_mode:?}"
            );
        }
    }

    #[test]
    fn sample_reports_sensor_error() {
        futures::executor::block_on(async {
            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut txns = init_transactions_app_mode(polling_mode);
            txns.push(status_read(STATUS_NO_DATA | STATUS_ERROR));
            let mut mock = Mock::new(&txns);

            let mut driver = Ccs811::new(&config(MeasMode::Every1s));
            driver.init(&mut mock).await.unwrap();
            let result = driver.sample(&mut mock).await;
            assert!(matches!(result, Err(Ccs811Error::SensorError)));

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
            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut txns = init_transactions_app_mode(polling_mode);
            txns.push(status_read(STATUS_NO_DATA));
            txns.push(status_read(STATUS_READY));
            txns.push(alg_result_read(600, 250));
            let mut mock = Mock::new(&txns);

            let pins: Ccs811Pins<ImmediateLow> = Ccs811Pins { nint: None };
            let mut driver = Ccs811::new_with_pins(&config(MeasMode::Every1s), pins);
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
            let irq_mode = meas_mode_register(MeasMode::Every1s, true);
            let mut txns = init_transactions_app_mode(irq_mode);
            txns.push(status_read(STATUS_READY));
            txns.push(alg_result_read(800, 150));
            let mut mock = Mock::new(&txns);

            let pins = Ccs811Pins {
                nint: Some(ImmediateLow),
            };
            let mut driver = Ccs811::new_with_pins(&config(MeasMode::Every1s), pins);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();

            assert!((r.eco2 - 800.0).abs() < 1.0, "eco2={}", r.eco2);
            assert!((r.tvoc - 150.0).abs() < 1.0, "tvoc={}", r.tvoc);

            mock.done();
        });
    }

    #[test]
    fn interrupt_path_still_checks_data_ready() {
        // NINT asserts but STATUS does not report DATA_READY yet — the driver
        // must keep polling STATUS rather than read a stale result.
        futures::executor::block_on(async {
            let irq_mode = meas_mode_register(MeasMode::Every1s, true);
            let mut txns = init_transactions_app_mode(irq_mode);
            txns.push(status_read(STATUS_NO_DATA));
            txns.push(status_read(STATUS_READY));
            txns.push(alg_result_read(800, 150));
            let mut mock = Mock::new(&txns);

            let pins = Ccs811Pins {
                nint: Some(ImmediateLow),
            };
            let mut driver = Ccs811::new_with_pins(&config(MeasMode::Every1s), pins);
            driver.init(&mut mock).await.unwrap();
            let r = driver.sample(&mut mock).await.unwrap();

            assert!((r.eco2 - 800.0).abs() < 1.0, "eco2={}", r.eco2);
            mock.done();
        });
    }

    #[test]
    fn reinit_recovers_after_first_bring_up() {
        // The generated source task re-runs init() to recover a degraded sensor.
        // The owned NINT pin (here None) is retained across re-inits.
        futures::executor::block_on(async {
            let polling_mode = meas_mode_register(MeasMode::Every1s, false);
            let mut txns = init_transactions_app_mode(polling_mode);
            txns.extend(init_transactions_app_mode(polling_mode)); // recovery re-init
            txns.push(status_read(STATUS_READY));
            txns.push(alg_result_read(700, 200));
            let mut mock = Mock::new(&txns);

            let mut driver = Ccs811::new(&config(MeasMode::Every1s));
            driver.init(&mut mock).await.unwrap();
            driver.init(&mut mock).await.unwrap(); // recovery re-init
            let r = driver.sample(&mut mock).await.unwrap();
            assert!((r.eco2 - 700.0).abs() < 1.0, "eco2={}", r.eco2);
            mock.done();
        });
    }
}
