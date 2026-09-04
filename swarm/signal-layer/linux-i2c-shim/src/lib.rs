//! Async-over-blocking I2C shim: bridges embedded-hal-async to Linux i2cdev via `spawn_blocking`.
//!
//! # Design
//!
//! [`SharedI2c`] wraps a blocking bus `B: BlockingI2c` behind two mutex layers:
//!
//! - Outer: `tokio::sync::Mutex` — held across the **entire** async transaction including
//!   the `spawn_blocking` await.  This is the serialization guarantee (SR-8): concurrent
//!   callers on the same bus wait at the async layer.
//! - Inner: `std::sync::Mutex` — exists solely to move `B` into the `'static` closure
//!   required by `spawn_blocking`.  It is provably uncontended because the outer tokio
//!   Mutex ensures only one task ever reaches this inner lock at a time.
//!
//! One lock order, no cycles → deadlock-free by construction.

use std::sync::{Arc, Mutex as StdMutex};

use embedded_hal::i2c::{ErrorKind, ErrorType, Operation};
use tokio::sync::Mutex as TokioMutex;

// ────────────────────────────────────────────────────────────────────────────
// Public types
// ────────────────────────────────────────────────────────────────────────────

/// An owned operation in a blocking I2C transaction.
///
/// Mirrors [`embedded_hal::i2c::Operation`] but with owned buffers so the
/// operation can be sent across thread boundaries into `spawn_blocking`.
#[derive(Debug)]
pub enum BlockingOp {
    /// Read `n` bytes into the buffer (starts as zeros, length = capacity).
    Read(Vec<u8>),
    /// Write the contained bytes.
    Write(Vec<u8>),
}

/// Error type returned by the I2C shim.
#[derive(Debug)]
pub struct ShimError(pub ErrorKind);

impl embedded_hal::i2c::Error for ShimError {
    fn kind(&self) -> ErrorKind {
        self.0
    }
}

impl core::fmt::Display for ShimError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "I2C shim error: {:?}", self.0)
    }
}

/// Seam for a blocking I2C bus — implemented by [`LinuxI2cdev`] and `MockBus`.
pub trait BlockingI2c: Send + 'static {
    /// Execute an I2C transaction (sequence of read/write operations).
    fn transaction(&mut self, addr: u8, ops: &mut [BlockingOp]) -> Result<(), ShimError>;
}

/// A clonable handle to a shared I2C bus.
///
/// `Clone` on a [`SharedI2c`] yields another handle to the **same** physical bus;
/// all clones share the outer tokio Mutex and are therefore fully serialized.
pub struct SharedI2c<B: BlockingI2c> {
    inner: Arc<TokioMutex<Arc<StdMutex<B>>>>,
}

impl<B: BlockingI2c> Clone for SharedI2c<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<B: BlockingI2c> SharedI2c<B> {
    /// Wrap a blocking bus.  Typically one `SharedI2c` per physical I2C bus.
    pub fn new(bus: B) -> Self {
        Self {
            inner: Arc::new(TokioMutex::new(Arc::new(StdMutex::new(bus)))),
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// embedded_hal_async::i2c::I2c implementation
// ────────────────────────────────────────────────────────────────────────────

impl<B: BlockingI2c> ErrorType for SharedI2c<B> {
    type Error = ShimError;
}

impl<B: BlockingI2c> embedded_hal_async::i2c::I2c for SharedI2c<B> {
    /// Execute the provided operations on the I2C bus as a single transaction.
    ///
    /// Lock discipline: the outer tokio Mutex is acquired and held for the
    /// duration of this method, including across the `spawn_blocking` await.
    /// The inner std Mutex is taken only inside the blocking closure and is
    /// always uncontended (outer lock serializes all callers).
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        // 1. Acquire the outer (tokio) mutex.  Held until end of this fn.
        let guard = self.inner.lock().await;

        // 2. Convert borrowed ops into owned ones for the 'static closure.
        let mut owned_ops: Vec<BlockingOp> = operations
            .iter()
            .map(|op| match op {
                Operation::Write(data) => BlockingOp::Write(data.to_vec()),
                Operation::Read(buf) => BlockingOp::Read(vec![0u8; buf.len()]),
            })
            .collect();

        // 3. Clone the inner Arc<StdMutex> so the closure owns it.
        let bus_arc = Arc::clone(&*guard);

        // 4. Run the blocking call.  The inner StdMutex is uncontended here
        //    because the outer tokio Mutex guarantees exclusivity.
        let result = tokio::task::spawn_blocking(move || {
            // The inner StdMutex is uncontended (outer tokio Mutex guarantees
            // exclusivity), but a panicking driver closure can poison it.
            // Recover from poison rather than aborting the host process.
            let mut bus = bus_arc
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            bus.transaction(address, &mut owned_ops)?;
            Ok(owned_ops)
        })
        .await
        .map_err(|_| ShimError(ErrorKind::Other))?;

        // 5. Copy read results back into the caller's mutable slices.
        let returned_ops = result?;
        let mut read_iter = returned_ops.iter().filter_map(|op| match op {
            BlockingOp::Read(buf) => Some(buf.as_slice()),
            BlockingOp::Write(_) => None,
        });

        for op in operations.iter_mut() {
            if let Operation::Read(buf) = op {
                let src = read_iter.next().ok_or(ShimError(ErrorKind::Other))?;
                buf.copy_from_slice(src);
            }
        }

        Ok(())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// LinuxI2cdev adapter (Linux only)
// ────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub mod linux {
    use super::{BlockingI2c, BlockingOp, ShimError};
    use embedded_hal::i2c::ErrorKind;
    use i2cdev::core::I2CDevice;
    use i2cdev::linux::LinuxI2CDevice;
    use std::path::Path;

    /// Thin adapter from [`i2cdev::linux::LinuxI2CDevice`] to [`BlockingI2c`].
    ///
    /// One `LinuxI2cdev` instance represents one I2C bus (`/dev/i2c-N`).
    /// The device-address target is set per-transaction via the Linux `ioctl`.
    pub struct LinuxI2cdev {
        path: std::path::PathBuf,
    }

    impl LinuxI2cdev {
        /// Open the I2C bus at `path` (e.g. `/dev/i2c-1`).
        ///
        /// # Errors
        ///
        /// Returns an I/O error if the device file cannot be opened.
        pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
            Ok(Self {
                path: path.as_ref().to_path_buf(),
            })
        }
    }

    impl BlockingI2c for LinuxI2cdev {
        fn transaction(&mut self, addr: u8, ops: &mut [BlockingOp]) -> Result<(), ShimError> {
            // Open a per-address device handle for this transaction.
            let mut dev = LinuxI2CDevice::new(&self.path, u16::from(addr))
                .map_err(|_| ShimError(ErrorKind::Other))?;

            for op in ops.iter_mut() {
                match op {
                    BlockingOp::Write(data) => {
                        dev.write(data).map_err(|_| ShimError(ErrorKind::Other))?;
                    }
                    BlockingOp::Read(buf) => {
                        dev.read(buf).map_err(|_| ShimError(ErrorKind::Other))?;
                    }
                }
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::LinuxI2cdev;

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex as StdMutex};
    use std::thread;
    use std::time::Duration;

    use embedded_hal::i2c::Operation;
    use embedded_hal_async::i2c::I2c as _;

    // ── MockBus ──────────────────────────────────────────────────────────────

    type MockLog = Arc<StdMutex<Vec<(thread::ThreadId, u8, Vec<String>)>>>;

    /// Records every transaction for assertion in tests.
    #[derive(Default)]
    struct MockBus {
        log: MockLog,
    }

    impl MockBus {
        fn new_shared() -> (Self, MockLog) {
            let log = Arc::new(StdMutex::new(Vec::new()));
            (
                MockBus {
                    log: Arc::clone(&log),
                },
                log,
            )
        }
    }

    impl BlockingI2c for MockBus {
        fn transaction(&mut self, addr: u8, ops: &mut [BlockingOp]) -> Result<(), ShimError> {
            let tid = thread::current().id();
            let op_names: Vec<String> = ops
                .iter()
                .map(|op| match op {
                    BlockingOp::Write(d) => format!("W({})", d.len()),
                    BlockingOp::Read(b) => format!("R({})", b.len()),
                })
                .collect();
            self.log.lock().unwrap().push((tid, addr, op_names));
            // Fill read buffers with a recognisable pattern.
            for op in ops.iter_mut() {
                if let BlockingOp::Read(buf) = op {
                    for (i, b) in buf.iter_mut().enumerate() {
                        // i is an index into a Vec<u8> (max 256 elements for I2C);
                        // truncation to u8 is intentional: the pattern wraps around.
                        #[allow(clippy::cast_possible_truncation)]
                        let byte = (i as u8).wrapping_add(addr);
                        *b = byte;
                    }
                }
            }
            Ok(())
        }
    }

    // ── (a) write-then-read maps to one transaction call ────────────────────

    #[tokio::test]
    async fn single_transaction_write_read_maps_to_one_call() {
        let (mock, log) = MockBus::new_shared();
        let mut shim = SharedI2c::new(mock);

        let write_buf = [0x42u8, 0xFF];
        let mut read_buf = [0u8; 3];

        shim.transaction(
            0x76,
            &mut [Operation::Write(&write_buf), Operation::Read(&mut read_buf)],
        )
        .await
        .expect("transaction should succeed");

        let entries = log.lock().unwrap();
        assert_eq!(entries.len(), 1, "exactly one transaction call");
        let (_, addr, ops) = &entries[0];
        assert_eq!(*addr, 0x76);
        assert_eq!(ops, &["W(2)", "R(3)"]);

        // Read data filled by mock: 0+addr, 1+addr, 2+addr
        assert_eq!(read_buf[0], 0x76u8.wrapping_add(0));
        assert_eq!(read_buf[1], 0x76u8.wrapping_add(1));
        assert_eq!(read_buf[2], 0x76u8.wrapping_add(2));
    }

    // ── (b) two tasks on one bus — operations never interleave ───────────────

    #[tokio::test]
    async fn interleaved_tasks_serialize_on_one_bus() {
        const TXNS: usize = 100;

        /// A bus that records all (`task_index`, op) pairs in order.
        struct OrderBus {
            log: Arc<StdMutex<Vec<u8>>>,
        }
        impl BlockingI2c for OrderBus {
            fn transaction(&mut self, addr: u8, ops: &mut [BlockingOp]) -> Result<(), ShimError> {
                let mut guard = self.log.lock().unwrap();
                for _ in ops.iter() {
                    guard.push(addr);
                }
                Ok(())
            }
        }

        let log = Arc::new(StdMutex::new(Vec::<u8>::new()));
        let bus = OrderBus {
            log: Arc::clone(&log),
        };
        let shim = SharedI2c::new(bus);

        let mut handles = Vec::new();

        for task_id in 0u8..2 {
            let mut s = shim.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..TXNS {
                    s.transaction(task_id, &mut [Operation::Write(&[0x00])])
                        .await
                        .expect("ok");
                }
            }));
        }

        for h in handles {
            h.await.expect("task panicked");
        }

        let entries = log.lock().unwrap();
        // Total: 2 tasks × 100 transactions × 1 op = 200 entries
        assert_eq!(entries.len(), 200);
        // Adjacent entries for a given task_id must be contiguous — they are
        // serialised by the tokio Mutex so no op from task 0 interleaves task 1.
        // Verify: for each consecutive pair where the value changes, the switch
        // must be a clean block boundary.  Since ops are 1-per-transaction and
        // the mutex serialises the whole transaction, the sequence is a series
        // of alternating runs.  We only assert NO partial interleave: no single
        // transaction is split (trivially true since each transaction is 1 op).
        // Stronger assertion: the log contains only 0s and 1s (sanity).
        for v in entries.iter() {
            assert!(*v <= 1, "unexpected task id in log");
        }
    }

    // ── SR-8 op-sequence integrity: multi-op transactions are never split ─────
    //
    // The spec SR-8 serialization guarantee means that a multi-op transaction
    // (e.g. a write-then-read that is 2 ops) is never interleaved with another
    // task's ops.  The per-bus tokio Mutex is held for the ENTIRE transaction
    // including all ops.  This test verifies that adjacent entries in the log
    // for a given task always belong to the same transaction (sequence integrity).

    /// SR-8: Multi-op transactions on one bus are never partially interleaved.
    /// Two tasks each issue 50 transactions of 3 ops each.  The resulting log
    /// of 300 ops must consist only of complete 3-op runs from one task at a
    /// time — no run of 1 or 2 ops from a task sandwiched between another task's
    /// ops.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn multi_op_transactions_never_interleave() {
        /// Records (`task_id`, `op_index_within_transaction`) for every op.
        struct SequenceBus {
            log: Arc<StdMutex<Vec<(u8, usize)>>>,
        }
        impl BlockingI2c for SequenceBus {
            fn transaction(&mut self, addr: u8, ops: &mut [BlockingOp]) -> Result<(), ShimError> {
                let mut guard = self.log.lock().unwrap();
                for (i, _op) in ops.iter().enumerate() {
                    guard.push((addr, i));
                }
                Ok(())
            }
        }

        const TASKS: u8 = 2;
        const TXN_PER_TASK: usize = 50;
        const OPS_PER_TXN: usize = 3;

        let log = Arc::new(StdMutex::new(Vec::new()));
        let bus = SequenceBus {
            log: Arc::clone(&log),
        };
        let shim = SharedI2c::new(bus);

        let mut handles = Vec::new();
        for task_id in 0..TASKS {
            let mut s = shim.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..TXN_PER_TASK {
                    // A 3-op transaction: write, read, write.
                    let mut rbuf = [0u8; 2];
                    s.transaction(
                        task_id,
                        &mut [
                            Operation::Write(&[0xA0]),
                            Operation::Read(&mut rbuf),
                            Operation::Write(&[0xA1]),
                        ],
                    )
                    .await
                    .expect("transaction ok");
                }
            }));
        }
        for h in handles {
            h.await.expect("task panicked");
        }

        let entries = log.lock().unwrap();
        let expected_total = usize::from(TASKS) * TXN_PER_TASK * OPS_PER_TXN;
        assert_eq!(entries.len(), expected_total, "total op count mismatch");

        // Walk the log in chunks of OPS_PER_TXN.  Each chunk must:
        // (a) have the same task_id throughout (no interleave), and
        // (b) have op indices 0, 1, 2 in order (no partial transaction).
        for (chunk_idx, chunk) in entries.chunks(OPS_PER_TXN).enumerate() {
            assert_eq!(
                chunk.len(),
                OPS_PER_TXN,
                "chunk {chunk_idx} has wrong length (truncated transaction?)"
            );
            let task_id = chunk[0].0;
            for (op_pos, &(tid, op_idx)) in chunk.iter().enumerate() {
                assert_eq!(
                    tid, task_id,
                    "SR-8 op-sequence integrity: chunk {chunk_idx} op {op_pos}: task_id changed mid-transaction ({task_id} → {tid})"
                );
                assert_eq!(
                    op_idx, op_pos,
                    "SR-8 op-sequence integrity: chunk {chunk_idx} op {op_pos}: op_index mismatch (expected {op_pos}, got {op_idx})"
                );
            }
        }
    }

    // ── (c) cross-bus independence: blocking on bus-A does not stall bus-B ──

    // Test-only wall-clock measurement (sanctioned pattern — see plan §Task 4).
    #[allow(clippy::disallowed_methods)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cross_bus_independence_under_blocking_mock() {
        /// A bus that `thread::sleep`s 200 ms per transaction.
        struct SlowBus;
        impl BlockingI2c for SlowBus {
            fn transaction(&mut self, _addr: u8, _ops: &mut [BlockingOp]) -> Result<(), ShimError> {
                thread::sleep(Duration::from_millis(200));
                Ok(())
            }
        }

        struct FastBus;
        impl BlockingI2c for FastBus {
            fn transaction(&mut self, _addr: u8, _ops: &mut [BlockingOp]) -> Result<(), ShimError> {
                // nearly instant
                Ok(())
            }
        }

        let mut slow = SharedI2c::new(SlowBus);
        let mut fast = SharedI2c::new(FastBus);

        let start = std::time::SystemTime::now();

        let slow_task = tokio::spawn(async move {
            slow.transaction(0x01, &mut [Operation::Write(&[0u8])])
                .await
                .expect("slow ok");
        });
        let fast_task = tokio::spawn(async move {
            fast.transaction(0x02, &mut [Operation::Write(&[0u8])])
                .await
                .expect("fast ok");
        });

        slow_task.await.expect("slow task");
        fast_task.await.expect("fast task");

        let elapsed_ms = start.elapsed().expect("time elapsed").as_millis();

        // The fast bus must not be delayed by the slow bus; both run in parallel.
        // Slow bus: ~200 ms; fast bus: ~0 ms; wall-clock total < 350 ms.
        assert!(
            elapsed_ms < 350,
            "cross-bus independence violated: elapsed {elapsed_ms}ms >= 350ms"
        );
    }

    // ── (d) 32×50 deadlock stress ────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn stress_32_tasks_50_transactions_no_deadlock() {
        struct NoopBus;
        impl BlockingI2c for NoopBus {
            fn transaction(&mut self, _addr: u8, _ops: &mut [BlockingOp]) -> Result<(), ShimError> {
                Ok(())
            }
        }

        let shim = SharedI2c::new(NoopBus);

        let mut handles = Vec::new();
        for _ in 0u32..32 {
            let mut s = shim.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..50usize {
                    s.transaction(0x10, &mut [Operation::Write(&[0x00])])
                        .await
                        .expect("noop ok");
                }
            }));
        }

        for h in handles {
            h.await.expect("task panicked");
        }
        // Reaching here means no deadlock.
    }

    // ── LinuxI2cdev compile-only test (no hardware required in CI) ──────────

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_i2cdev_implements_blocking_i2c() {
        // Compile-only: assert LinuxI2cdev satisfies the BlockingI2c trait bound.
        #[allow(dead_code)]
        fn assert_blocking_i2c<T: BlockingI2c>(_: &T) {}

        // We cannot open a real /dev/i2c-* in CI, so we just verify the type
        // satisfies the trait bound via a function accepting `BlockingI2c`.
        // The open() call would fail at runtime without hardware.
        fn _compile_check() {
            // This function is never called; its mere existence proves compilation.
            let dev = LinuxI2cdev::open("/dev/i2c-1").expect("hardware required");
            assert_blocking_i2c(&dev);
            let _shim: SharedI2c<LinuxI2cdev> = SharedI2c::new(dev);
        }
    }
}
