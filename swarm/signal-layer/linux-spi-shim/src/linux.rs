//! spidev adapter (Linux only): a [`BlockingSpiBus`] over `/dev/spidevB.C`.
//!
//! The node is opened with `SPI_NO_CS` where the controller supports it, so
//! the kernel never drives any chip-select - CS is owned by
//! [`SharedSpiDevice`](crate::SharedSpiDevice) as a GPIO line, keeping the
//! manifest's per-device `cs` pin semantics identical to the ESP backend. On a
//! controller that rejects `SPI_NO_CS` (e.g. the Pi 5's RP1 SPI), the shim
//! falls back to leaving the kernel CS enabled; its CE line then toggles a pin
//! no device is wired to, while the GPIO CS still selects the device.

use std::io;
use std::path::Path;

use spidev::{SpiModeFlags, Spidev, SpidevOptions, SpidevTransfer};

use crate::bus::{BlockingOp, BlockingSpiBus, ShimSpiError};

use embedded_hal::spi::ErrorKind;

/// `EINVAL`, returned by the mode ioctl when the controller does not advertise
/// `SPI_NO_CS` in its `mode_bits`.
const EINVAL: i32 = 22;

/// One spidev-backed SPI bus (e.g. `/dev/spidev0.0`), kernel CS disabled.
pub struct LinuxSpidev {
    dev: Spidev,
}

impl LinuxSpidev {
    /// Open the spidev node at `path` with the given clock and SPI mode
    /// (0-3), 8-bit words, and the kernel chip-select disabled where the
    /// controller supports it (see the module docs for the fallback).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the node cannot be opened or configured
    /// (missing device, insufficient permissions, or a mode/clock the
    /// controller rejects for a reason other than `SPI_NO_CS`).
    pub fn open(path: impl AsRef<Path>, freq_hz: u32, mode: u8) -> io::Result<Self> {
        let mut dev = Spidev::open(path)?;
        let mode_flags = match mode {
            0 => SpiModeFlags::SPI_MODE_0,
            1 => SpiModeFlags::SPI_MODE_1,
            2 => SpiModeFlags::SPI_MODE_2,
            3 => SpiModeFlags::SPI_MODE_3,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid SPI mode {other}: expected 0..=3"),
                ));
            }
        };
        let options = |extra| {
            SpidevOptions::new()
                .bits_per_word(8)
                .max_speed_hz(freq_hz)
                .mode(mode_flags | extra)
                .build()
        };

        // Prefer disabling the kernel chip-select. A controller whose driver does not advertise
        // `SPI_NO_CS` in its `mode_bits` rejects that bit with `EINVAL`; since each device owns its
        // CS as a GPIO line, retry without it and let the kernel CS toggle a pin no device is wired
        // to. An `EINVAL` raised for an unrelated reason (an unsupported mode or clock) also fails
        // the retry, which keeps the same mode and clock, so it surfaces there instead of being
        // masked as missing `SPI_NO_CS` support.
        if let Err(e) = dev.configure(&options(SpiModeFlags::SPI_NO_CS)) {
            if e.raw_os_error() == Some(EINVAL) {
                dev.configure(&options(SpiModeFlags::empty())).map_err(|retry_err| {
                    io::Error::new(
                        retry_err.kind(),
                        format!(
                            "SPI configuration rejected (mode {mode}, {freq_hz} Hz): {retry_err}"
                        ),
                    )
                })?;
            } else {
                return Err(e);
            }
        }

        Ok(Self { dev })
    }
}

impl BlockingSpiBus for LinuxSpidev {
    fn transaction(&mut self, ops: &mut [BlockingOp]) -> Result<(), ShimSpiError> {
        for op in ops.iter_mut() {
            let result = match op {
                BlockingOp::Read(buf) => self.dev.transfer(&mut SpidevTransfer::read(buf)),
                BlockingOp::Write(data) => self.dev.transfer(&mut SpidevTransfer::write(data)),
                BlockingOp::Transfer { tx, rx } => {
                    self.dev.transfer(&mut SpidevTransfer::read_write(tx, rx))
                }
                BlockingOp::TransferInPlace(buf) => {
                    let tx = buf.clone();
                    self.dev.transfer(&mut SpidevTransfer::read_write(&tx, buf))
                }
                BlockingOp::DelayNs(ns) => {
                    // In-transaction delay with CS held asserted; we are on a
                    // spawn_blocking thread, so sleeping here is fine.
                    std::thread::sleep(std::time::Duration::from_nanos(u64::from(*ns)));
                    Ok(())
                }
            };
            result.map_err(|_| ShimSpiError(ErrorKind::Other))?;
        }
        Ok(())
    }
}
