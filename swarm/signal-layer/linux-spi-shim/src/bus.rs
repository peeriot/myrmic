//! Shared SPI bus and software-CS device handles.
//!
//! [`SharedSpiBus`] wraps a blocking bus `B: BlockingSpiBus` behind two mutex
//! layers, exactly like `linux-i2c-shim`'s `SharedI2c`:
//!
//! - Outer: `tokio::sync::Mutex` — held across the **entire** async
//!   transaction including the `spawn_blocking` await and both CS edges. This
//!   is the serialization guarantee: two devices on one bus can never
//!   interleave, so a chip-select is never asserted inside another device's
//!   transaction.
//! - Inner: `std::sync::Mutex` — exists solely to move `B` into the `'static`
//!   closure required by `spawn_blocking`; provably uncontended under the
//!   outer lock.
//!
//! [`SharedSpiDevice`] adds the chip-select: CS is asserted (driven low)
//! after the bus lock is taken and deasserted before it is released — on the
//! error path too. The kernel's own CS is disabled (`SPI_NO_CS`), so the CS
//! line is whatever `OutputPin` the manifest wired (a GPIO character-device
//! line in generated pipelines).

use std::sync::{Arc, Mutex as StdMutex};

use embedded_hal::digital::OutputPin;
use embedded_hal::spi::{ErrorKind, ErrorType, Operation};
use tokio::sync::Mutex as TokioMutex;

// ────────────────────────────────────────────────────────────────────────────
// Public types
// ────────────────────────────────────────────────────────────────────────────

/// An owned operation in a blocking SPI transaction.
///
/// Mirrors [`embedded_hal::spi::Operation`] but with owned buffers so the
/// operation can be sent across thread boundaries into `spawn_blocking`.
#[derive(Debug)]
pub enum BlockingOp {
    /// Read `n` bytes (buffer starts as zeros, length = capacity).
    Read(Vec<u8>),
    /// Write the contained bytes.
    Write(Vec<u8>),
    /// Full-duplex transfer: write `tx`, fill `rx`. Equal lengths — the shim
    /// pads the shorter side per the `embedded-hal` transfer contract.
    Transfer { tx: Vec<u8>, rx: Vec<u8> },
    /// Full-duplex transfer reusing one buffer.
    TransferInPlace(Vec<u8>),
    /// In-transaction delay (CS stays asserted).
    DelayNs(u32),
}

/// Error type returned by the SPI shim.
#[derive(Debug)]
pub struct ShimSpiError(pub ErrorKind);

impl embedded_hal::spi::Error for ShimSpiError {
    fn kind(&self) -> ErrorKind {
        self.0
    }
}

impl core::fmt::Display for ShimSpiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "SPI shim error: {:?}", self.0)
    }
}

/// Seam for a blocking chip-select-less SPI bus — implemented by
/// [`LinuxSpidev`](crate::LinuxSpidev) and test mocks.
pub trait BlockingSpiBus: Send + 'static {
    /// Execute a sequence of SPI operations with the chip already selected.
    fn transaction(&mut self, ops: &mut [BlockingOp]) -> Result<(), ShimSpiError>;
}

/// A clonable handle to a shared SPI bus.
///
/// `Clone` yields another handle to the **same** physical bus; all clones
/// share the outer tokio Mutex and are therefore fully serialized.
pub struct SharedSpiBus<B: BlockingSpiBus> {
    inner: Arc<TokioMutex<Arc<StdMutex<B>>>>,
}

impl<B: BlockingSpiBus> Clone for SharedSpiBus<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<B: BlockingSpiBus> SharedSpiBus<B> {
    /// Wrap a blocking bus. Typically one `SharedSpiBus` per spidev node.
    pub fn new(bus: B) -> Self {
        Self {
            inner: Arc::new(TokioMutex::new(Arc::new(StdMutex::new(bus)))),
        }
    }

    /// Bind a device to this bus with its chip-select line.
    pub fn device<CS: OutputPin>(&self, cs: CS) -> SharedSpiDevice<B, CS> {
        SharedSpiDevice {
            bus: self.clone(),
            cs,
        }
    }
}

/// One SPI device on a [`SharedSpiBus`], owning its (active-low) CS line.
pub struct SharedSpiDevice<B: BlockingSpiBus, CS: OutputPin> {
    bus: SharedSpiBus<B>,
    cs: CS,
}

// ────────────────────────────────────────────────────────────────────────────
// embedded_hal_async::spi::SpiDevice implementation
// ────────────────────────────────────────────────────────────────────────────

impl<B: BlockingSpiBus, CS: OutputPin> ErrorType for SharedSpiDevice<B, CS> {
    type Error = ShimSpiError;
}

impl<B: BlockingSpiBus, CS: OutputPin + Send> embedded_hal_async::spi::SpiDevice
    for SharedSpiDevice<B, CS>
{
    /// Execute the operations as one chip-selected transaction.
    ///
    /// Lock discipline: the outer tokio Mutex is held for the whole method —
    /// CS assert, the `spawn_blocking` await, and CS deassert — so no other
    /// device's transaction can interleave. CS is deasserted on every path,
    /// including bus errors and a panicked blocking task.
    async fn transaction(
        &mut self,
        operations: &mut [Operation<'_, u8>],
    ) -> Result<(), Self::Error> {
        // 1. Acquire the outer (tokio) mutex. Held until end of this fn.
        let guard = self.bus.inner.lock().await;

        // 2. Convert borrowed ops into owned ones for the 'static closure.
        //    Transfer pads the shorter side to the longer length (the
        //    embedded-hal contract: missing write bytes are sent as 0x00,
        //    surplus read bytes are discarded).
        let mut owned_ops: Vec<BlockingOp> = operations
            .iter()
            .map(|op| match op {
                Operation::Read(buf) => BlockingOp::Read(vec![0u8; buf.len()]),
                Operation::Write(data) => BlockingOp::Write(data.to_vec()),
                Operation::Transfer(read, write) => {
                    let n = read.len().max(write.len());
                    let mut tx = vec![0u8; n];
                    tx[..write.len()].copy_from_slice(write);
                    BlockingOp::Transfer {
                        tx,
                        rx: vec![0u8; n],
                    }
                }
                Operation::TransferInPlace(buf) => BlockingOp::TransferInPlace(buf.to_vec()),
                Operation::DelayNs(ns) => BlockingOp::DelayNs(*ns),
            })
            .collect();

        // 3. Assert CS (active low) only after the bus is exclusively ours.
        self.cs
            .set_low()
            .map_err(|_| ShimSpiError(ErrorKind::ChipSelectFault))?;

        // 4. Run the blocking transfer while CS is held asserted.
        let bus_arc = Arc::clone(&*guard);
        let joined = tokio::task::spawn_blocking(move || {
            // Uncontended under the outer lock; recover from poison rather
            // than aborting the host process.
            let mut bus = bus_arc
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            bus.transaction(&mut owned_ops)?;
            Ok(owned_ops)
        })
        .await;

        // 5. Deassert CS on EVERY path before any error propagates.
        let cs_release = self.cs.set_high();

        let returned_ops = joined.map_err(|_| ShimSpiError(ErrorKind::Other))??;
        cs_release.map_err(|_| ShimSpiError(ErrorKind::ChipSelectFault))?;

        // 6. Copy read results back into the caller's mutable slices.
        let mut results = returned_ops.into_iter();
        for op in operations.iter_mut() {
            let owned = results.next().ok_or(ShimSpiError(ErrorKind::Other))?;
            match (op, owned) {
                (Operation::Read(buf), BlockingOp::Read(data)) => {
                    buf.copy_from_slice(&data);
                }
                (Operation::Transfer(read, _), BlockingOp::Transfer { rx, .. }) => {
                    read.copy_from_slice(&rx[..read.len()]);
                }
                (Operation::TransferInPlace(buf), BlockingOp::TransferInPlace(data)) => {
                    buf.copy_from_slice(&data);
                }
                (Operation::Write(_), BlockingOp::Write(_))
                | (Operation::DelayNs(_), BlockingOp::DelayNs(_)) => {}
                _ => return Err(ShimSpiError(ErrorKind::Other)),
            }
        }

        Ok(())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Duration;

    use embedded_hal::digital::ErrorType as PinErrorType;
    use embedded_hal_async::spi::SpiDevice as _;

    /// Shared event log: CS edges and bus transactions, in order.
    type Log = Arc<StdMutex<Vec<String>>>;

    struct MockBus {
        log: Log,
        name: &'static str,
        /// Written into every Read/Transfer rx byte.
        fill: u8,
        fail: bool,
        delay: Option<Duration>,
    }

    impl BlockingSpiBus for MockBus {
        fn transaction(&mut self, ops: &mut [BlockingOp]) -> Result<(), ShimSpiError> {
            if let Some(d) = self.delay {
                std::thread::sleep(d);
            }
            if self.fail {
                self.log.lock().unwrap().push(format!("{}:fail", self.name));
                return Err(ShimSpiError(ErrorKind::Other));
            }
            for op in ops.iter_mut() {
                match op {
                    BlockingOp::Read(buf) => buf.fill(self.fill),
                    BlockingOp::Transfer { rx, .. } => rx.fill(self.fill),
                    BlockingOp::TransferInPlace(buf) => buf.fill(self.fill),
                    BlockingOp::Write(_) | BlockingOp::DelayNs(_) => {}
                }
            }
            self.log.lock().unwrap().push(format!("{}:tx", self.name));
            Ok(())
        }
    }

    struct MockCs {
        log: Log,
        name: &'static str,
    }

    #[derive(Debug)]
    struct NeverPinError;
    impl embedded_hal::digital::Error for NeverPinError {
        fn kind(&self) -> embedded_hal::digital::ErrorKind {
            embedded_hal::digital::ErrorKind::Other
        }
    }
    impl PinErrorType for MockCs {
        type Error = NeverPinError;
    }
    impl OutputPin for MockCs {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:cs_low", self.name));
            Ok(())
        }
        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:cs_high", self.name));
            Ok(())
        }
    }

    fn rig(fill: u8, fail: bool, delay: Option<Duration>) -> (Log, SharedSpiBus<MockBus>) {
        let log: Log = Arc::default();
        let bus = SharedSpiBus::new(MockBus {
            log: Arc::clone(&log),
            name: "bus",
            fill,
            fail,
            delay,
        });
        (log, bus)
    }

    #[tokio::test]
    async fn cs_wraps_the_transaction_in_order() {
        let (log, bus) = rig(0xAB, false, None);
        let mut dev = bus.device(MockCs {
            log: Arc::clone(&log),
            name: "a",
        });

        let mut buf = [0u8; 3];
        dev.transaction(&mut [Operation::Read(&mut buf)])
            .await
            .unwrap();

        assert_eq!(buf, [0xAB; 3]);
        assert_eq!(
            log.lock().unwrap().as_slice(),
            ["a:cs_low", "bus:tx", "a:cs_high"],
            "CS must assert before and deassert after the bus transaction"
        );
    }

    #[tokio::test]
    async fn cs_is_released_when_the_bus_fails() {
        let (log, bus) = rig(0, true, None);
        let mut dev = bus.device(MockCs {
            log: Arc::clone(&log),
            name: "a",
        });

        let result = dev.transaction(&mut [Operation::Write(&[1])]).await;
        assert!(result.is_err(), "bus failure must propagate");
        assert_eq!(
            log.lock().unwrap().last().map(String::as_str),
            Some("a:cs_high"),
            "CS must be deasserted on the error path"
        );
    }

    #[tokio::test]
    async fn transfer_pads_write_and_truncates_read() {
        let (_log, bus) = rig(0x55, false, None);
        let mut dev = bus.device(MockCs {
            log: Arc::default(),
            name: "a",
        });

        // read shorter than write: surplus rx bytes are discarded.
        let mut short_read = [0u8; 2];
        dev.transaction(&mut [Operation::Transfer(&mut short_read, &[9, 9, 9, 9])])
            .await
            .unwrap();
        assert_eq!(short_read, [0x55; 2]);

        // read longer than write: the transfer still fills the whole read.
        let mut long_read = [0u8; 4];
        dev.transaction(&mut [Operation::Transfer(&mut long_read, &[7])])
            .await
            .unwrap();
        assert_eq!(long_read, [0x55; 4]);
    }

    /// Two devices on one bus: transactions never interleave — every
    /// `cs_low..cs_high` window contains exactly its own bus transaction.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_devices_are_serialized() {
        let (log, bus) = rig(0, false, Some(Duration::from_millis(20)));
        let mut dev_a = bus.device(MockCs {
            log: Arc::clone(&log),
            name: "a",
        });
        let mut dev_b = bus.device(MockCs {
            log: Arc::clone(&log),
            name: "b",
        });

        let ta = tokio::spawn(async move {
            for _ in 0..3 {
                dev_a
                    .transaction(&mut [Operation::Write(&[1])])
                    .await
                    .unwrap();
            }
        });
        let tb = tokio::spawn(async move {
            for _ in 0..3 {
                dev_b
                    .transaction(&mut [Operation::Write(&[2])])
                    .await
                    .unwrap();
            }
        });
        ta.await.unwrap();
        tb.await.unwrap();

        let entries = log.lock().unwrap();
        for window in entries.chunks(3) {
            let owner = window[0].split(':').next().unwrap();
            assert_eq!(window[0], format!("{owner}:cs_low"));
            assert_eq!(window[1], "bus:tx");
            assert_eq!(window[2], format!("{owner}:cs_high"));
        }
    }
}
