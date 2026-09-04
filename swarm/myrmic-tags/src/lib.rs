//! Collection of myrmic tags
#![no_std]

extern crate alloc;

pub const TAG_LINUX: &str = "linux";

/// Capability tag advertised by a node that can drive a BLE peripheral.
pub const TAG_BLE: &str = "ble";

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// List of platforms from which tags can be obtained
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Esp32c5,
    Esp32c6,
    Esp32c61,
}

impl Platform {
    /// Gets the tags for the platform
    pub fn get_tags(&self) -> Vec<&'static str> {
        match self {
            Self::Linux => vec![TAG_LINUX],
            Self::Esp32c5 => vec!["esp32c5", "esp32", "riscv32imac", "embedded"],
            Self::Esp32c6 => vec!["esp32c6", "esp32", "riscv32imac", "embedded"],
            Self::Esp32c61 => vec!["esp32c61", "esp32", "riscv32imac", "embedded"],
        }
    }
}

impl core::convert::TryFrom<&str> for Platform {
    type Error = UnknownPlatform;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            TAG_LINUX => Ok(Self::Linux),
            "esp32c5" => Ok(Self::Esp32c5),
            "esp32c6" => Ok(Self::Esp32c6),
            "esp32c61" => Ok(Self::Esp32c61),
            _ => Err(UnknownPlatform(String::from(s))),
        }
    }
}

/// Error type for `TryFrom`
#[derive(Debug)]
pub struct UnknownPlatform(pub String);
