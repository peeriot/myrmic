//! Synthetic signal-layer source for hardware-in-the-loop tests.
//!
//! This is **not** a real sensor. It satisfies the codegen driver contract
//! (`new` / `init` / `sample` over `embedded_hal_async::i2c::I2c`) but ignores
//! the bus entirely and produces a deterministic, bounded sawtooth ramp. It
//! lets the dataplane HIL suite assert on exact tap values, moving-average
//! output, and threshold-trigger alarms without depending on any physical
//! sensor being wired to the rig.
//!
//! # Ramp
//!
//! Starting at [`SimSourceConfig::start`], each [`SimSource::sample`] returns
//! the current value and then advances by [`SimSourceConfig::step`]. When the
//! value would exceed [`SimSourceConfig::max`] it wraps back to `start`. With
//! the defaults (`start = 0`, `step = 1`, `max = 100`) the sequence is
//! `0, 1, 2, …, 100, 0, 1, …`. Because the ramp is monotonic within a cycle, a
//! downstream `threshold-trigger` fires exactly once per cycle at a
//! predictable value.
//!
//! `init` and `sample` never fail, so the source never enters the health
//! state machine's `Degraded`/`Down` path — health-failure coverage comes from
//! a real driver with no sensor populated (see the HIL board manifest).

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::i2c::I2c;

/// Configuration for the synthetic ramp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimSourceConfig {
    /// Value emitted on the first sample and the value the ramp wraps back to.
    pub start: f32,
    /// Amount added to the value after each sample.
    pub step: f32,
    /// Upper bound. Once the value would exceed this, it wraps back to `start`.
    pub max: f32,
}

impl Default for SimSourceConfig {
    /// `0, 1, 2, …, 100, 0, …`.
    fn default() -> Self {
        Self {
            start: 0.0,
            step: 1.0,
            max: 100.0,
        }
    }
}

/// One synthetic reading.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimSourceReadings {
    /// Current position on the ramp.
    pub value: f32,
}

/// Errors returned by the synthetic source.
///
/// The source never fails; the variant exists only so the `init`/`sample`
/// return types match the codegen driver contract (`Result<_, E<I::Error>>`).
#[non_exhaustive]
#[derive(Debug)]
pub enum SimSourceError<E: core::fmt::Debug> {
    /// Underlying I2C bus error. Never produced by this driver.
    Bus(E),
}

impl<E: core::fmt::Debug> From<E> for SimSourceError<E> {
    fn from(e: E) -> Self {
        Self::Bus(e)
    }
}

/// Synthetic source instance.
pub struct SimSource {
    cfg: SimSourceConfig,
    next: f32,
}

impl SimSource {
    /// Construct a source primed to emit [`SimSourceConfig::start`] first.
    /// Touches no bus (infallible), matching the driver contract.
    #[must_use]
    pub fn new(cfg: &SimSourceConfig) -> Self {
        Self {
            cfg: *cfg,
            next: cfg.start,
        }
    }

    /// No-op bring-up. Always succeeds — the bus is ignored.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    // `async` is unused here but mandated by the codegen driver contract: the
    // generated source task calls `driver.init(&mut bus).await`.
    #[allow(clippy::unused_async)]
    pub async fn init<I: I2c>(&mut self, _bus: &mut I) -> Result<(), SimSourceError<I::Error>> {
        log::info!("[sim-source] init OK (synthetic)");
        Ok(())
    }

    /// Return the current ramp value, then advance (wrapping at `max`).
    /// The bus is ignored.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    // `async` is unused here but mandated by the codegen driver contract: the
    // generated source task calls `driver.sample(&mut bus).await`.
    #[allow(clippy::unused_async)]
    pub async fn sample<I: I2c>(
        &mut self,
        _bus: &mut I,
    ) -> Result<SimSourceReadings, SimSourceError<I::Error>> {
        let value = self.next;
        let advanced = self.next + self.cfg.step;
        self.next = if advanced > self.cfg.max {
            self.cfg.start
        } else {
            advanced
        };
        log::debug!("[sim-source] value={value}");
        Ok(SimSourceReadings { value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal_mock::eh1::i2c::Mock;

    /// A bus that must never be touched — no transactions are queued, so any
    /// read/write would panic. Proves the synthetic source ignores the bus.
    fn untouched_bus() -> Mock {
        Mock::new(&[])
    }

    #[test]
    fn ramps_deterministically_and_ignores_bus() {
        futures::executor::block_on(async {
            let mut bus = untouched_bus();
            let cfg = SimSourceConfig {
                start: 0.0,
                step: 1.0,
                max: 3.0,
            };
            let mut src = SimSource::new(&cfg);
            src.init(&mut bus).await.unwrap();

            let mut seen = Vec::new();
            for _ in 0..6 {
                seen.push(src.sample(&mut bus).await.unwrap().value);
            }
            // 0,1,2,3 then wrap to 0,1
            assert_eq!(seen, vec![0.0, 1.0, 2.0, 3.0, 0.0, 1.0]);

            bus.done();
        });
    }

    #[test]
    fn wraps_when_step_overshoots_max() {
        futures::executor::block_on(async {
            let mut bus = untouched_bus();
            let cfg = SimSourceConfig {
                start: 10.0,
                step: 4.0,
                max: 15.0,
            };
            let mut src = SimSource::new(&cfg);

            // 10, 14, then 18 > 15 → wrap to 10, 14
            let mut seen = Vec::new();
            for _ in 0..4 {
                seen.push(src.sample(&mut bus).await.unwrap().value);
            }
            assert_eq!(seen, vec![10.0, 14.0, 10.0, 14.0]);

            bus.done();
        });
    }

    #[test]
    fn crosses_a_threshold_exactly_once_per_cycle() {
        futures::executor::block_on(async {
            let mut bus = untouched_bus();
            let cfg = SimSourceConfig {
                start: 0.0,
                step: 1.0,
                max: 5.0,
            };
            let mut src = SimSource::new(&cfg);

            // One full cycle is 0..=5; values >= 3 in a cycle are {3,4,5}, but a
            // rising-edge threshold at 3 fires once (the 2->3 transition).
            let mut crossings = 0;
            let mut prev = f32::NEG_INFINITY;
            for _ in 0..6 {
                let v = src.sample(&mut bus).await.unwrap().value;
                if prev < 3.0 && v >= 3.0 {
                    crossings += 1;
                }
                prev = v;
            }
            assert_eq!(crossings, 1);

            bus.done();
        });
    }
}
