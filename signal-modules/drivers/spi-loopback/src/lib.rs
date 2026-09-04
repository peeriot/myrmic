//! SPI loopback test source — the SPI counterpart of `sim-source`.
//!
//! This is **not** a real sensor. Each [`SpiLoopback::sample`] full-duplex
//! transfers an incrementing byte pattern over the bus and compares the echo:
//! with MOSI wired to MISO, `echo_ok` reads `1.0` and proves the entire SPI
//! path — spidev/peripheral, mode, clock, and chip-select wiring — on real
//! hardware. Without the jumper (or with broken wiring) it reads `0.0`.
//!
//! `counter` increments per sample, so a downstream tap shows liveness even
//! when the jumper is absent.

#![cfg_attr(not(test), no_std)]

use embedded_hal_async::spi::SpiDevice;

/// Longest supported transfer, bounding the stack buffers.
const MAX_TRANSFER: usize = 32;

/// Configuration for the loopback pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiLoopbackConfig {
    /// Bytes per loopback transfer, clamped to `1..=32`.
    pub transfer_len: u8,
}

impl Default for SpiLoopbackConfig {
    fn default() -> Self {
        Self { transfer_len: 8 }
    }
}

/// One loopback probe result.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpiLoopbackReadings {
    /// `1.0` when the echoed bytes matched the written pattern, else `0.0`.
    pub echo_ok: f32,
    /// Number of samples taken so far (wraps at `u16::MAX`).
    pub counter: f32,
}

/// Errors returned by the loopback source.
#[non_exhaustive]
#[derive(Debug)]
pub enum SpiLoopbackError<E: core::fmt::Debug> {
    /// Underlying SPI bus error.
    Bus(E),
}

impl<E: core::fmt::Debug> From<E> for SpiLoopbackError<E> {
    fn from(e: E) -> Self {
        Self::Bus(e)
    }
}

/// Loopback source instance.
pub struct SpiLoopback {
    len: usize,
    counter: u16,
}

impl SpiLoopback {
    /// Construct the source. Touches no bus (infallible), matching the driver
    /// contract; `transfer_len` is clamped to `1..=32`.
    #[must_use]
    pub fn new(cfg: &SpiLoopbackConfig) -> Self {
        Self {
            len: usize::from(cfg.transfer_len).clamp(1, MAX_TRANSFER),
            counter: 0,
        }
    }

    /// Probe the bus once so a broken wiring surfaces at bring-up.
    ///
    /// # Errors
    ///
    /// Returns the underlying SPI error if the transfer fails.
    pub async fn init<S: SpiDevice>(
        &mut self,
        spi: &mut S,
    ) -> Result<(), SpiLoopbackError<S::Error>> {
        let (tx, mut rx) = ([0u8; MAX_TRANSFER], [0u8; MAX_TRANSFER]);
        spi.transfer(&mut rx[..self.len], &tx[..self.len]).await?;
        log::info!("[spi-loopback] init OK ({} byte transfers)", self.len);
        Ok(())
    }

    /// Transfer the next pattern and report whether the echo matched.
    ///
    /// # Errors
    ///
    /// Returns the underlying SPI error if the transfer fails.
    pub async fn sample<S: SpiDevice>(
        &mut self,
        spi: &mut S,
    ) -> Result<SpiLoopbackReadings, SpiLoopbackError<S::Error>> {
        self.counter = self.counter.wrapping_add(1);

        let mut tx = [0u8; MAX_TRANSFER];
        #[allow(clippy::cast_possible_truncation)] // low byte is the point
        let seed = (self.counter & 0xFF) as u8;
        for (i, byte) in tx[..self.len].iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation)] // i < MAX_TRANSFER = 32
            let offset = i as u8;
            *byte = seed.wrapping_add(offset);
        }

        let mut rx = [0u8; MAX_TRANSFER];
        spi.transfer(&mut rx[..self.len], &tx[..self.len]).await?;

        let matched = rx[..self.len] == tx[..self.len];
        log::debug!(
            "[spi-loopback] #{} echo {}",
            self.counter,
            if matched { "ok" } else { "MISMATCH" }
        );
        Ok(SpiLoopbackReadings {
            echo_ok: if matched { 1.0 } else { 0.0 },
            counter: f32::from(self.counter),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use embedded_hal_mock::eh1::spi::{Mock, Transaction};

    #[test]
    fn echo_match_reads_one() {
        futures::executor::block_on(async {
            let mut drv = SpiLoopback::new(&SpiLoopbackConfig { transfer_len: 4 });
            // counter=1 → pattern [1, 2, 3, 4]; mock echoes it back verbatim.
            let pattern = vec![1u8, 2, 3, 4];
            let mut spi = Mock::new(&[
                Transaction::transaction_start(),
                Transaction::transfer(pattern.clone(), pattern),
                Transaction::transaction_end(),
            ]);

            let r = drv.sample(&mut spi).await.unwrap();
            assert!((r.echo_ok - 1.0).abs() < f32::EPSILON);
            assert!((r.counter - 1.0).abs() < f32::EPSILON);
            spi.done();
        });
    }

    #[test]
    fn echo_mismatch_reads_zero() {
        futures::executor::block_on(async {
            let mut drv = SpiLoopback::new(&SpiLoopbackConfig { transfer_len: 2 });
            // No jumper: MISO floats — mock returns zeros.
            let mut spi = Mock::new(&[
                Transaction::transaction_start(),
                Transaction::transfer(vec![1, 2], vec![0, 0]),
                Transaction::transaction_end(),
            ]);

            let r = drv.sample(&mut spi).await.unwrap();
            assert!(r.echo_ok.abs() < f32::EPSILON);
            spi.done();
        });
    }

    #[test]
    fn transfer_len_is_clamped() {
        let drv = SpiLoopback::new(&SpiLoopbackConfig { transfer_len: 0 });
        assert_eq!(drv.len, 1);
        let drv = SpiLoopback::new(&SpiLoopbackConfig { transfer_len: 200 });
        assert_eq!(drv.len, MAX_TRANSFER);
    }
}
