//! Async-over-blocking SPI shim: bridges `embedded-hal-async`'s [`SpiDevice`]
//! to Linux spidev with software chip-select, so the platform-agnostic SPI
//! drivers run unchanged on Linux.
//!
//! # Design
//!
//! The kernel's own chip-select is disabled (`SPI_NO_CS`); each device owns
//! its CS line as an [`embedded_hal::digital::OutputPin`] and the shim asserts
//! it around the whole transaction — the same bus-plus-CS composition the ESP
//! backend uses, and the software-CS `SpiDevice` pattern from
//! `embedded-hal-bus`, made async with the two-mutex `spawn_blocking`
//! structure proven in `linux-i2c-shim`.
//!
//! [`SpiDevice`]: embedded_hal_async::spi::SpiDevice

mod bus;
#[cfg(target_os = "linux")]
mod linux;

pub use bus::{BlockingOp, BlockingSpiBus, SharedSpiBus, SharedSpiDevice, ShimSpiError};
#[cfg(target_os = "linux")]
pub use linux::LinuxSpidev;
