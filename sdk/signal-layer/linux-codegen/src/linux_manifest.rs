//! Linux-specific manifest extensions: parses the same YAML as the common
//! `BoardManifest` but additionally reads the fields only Linux needs — the
//! `dev_path` of each I2C bus, and the GPIO/PWM chip each output device's
//! pins live on.

use indexmap::IndexMap;
use serde::Deserialize;

/// Minimal overlay parsed from the same manifest YAML to extract
/// Linux-specific fields that `BoardManifest` does not carry.
#[derive(Debug, Clone, Deserialize)]
pub struct LinuxManifestOverlay {
    #[serde(default)]
    pub buses: IndexMap<String, LinuxBusOverlay>,
    #[serde(default)]
    pub devices: Vec<LinuxDeviceOverlay>,
}

impl LinuxManifestOverlay {
    /// Look up the overlay entry for a device by its manifest `id`.
    pub fn device(&self, id: &str) -> Option<&LinuxDeviceOverlay> {
        self.devices.iter().find(|d| d.id == id)
    }
}

/// Linux-specific extras for one bus.
#[derive(Debug, Clone, Deserialize)]
pub struct LinuxBusOverlay {
    /// Path to the Linux I2C character device, e.g. `/dev/i2c-1`.
    /// Required for every I2C bus on Linux; omitted for SPI buses.
    #[serde(default)]
    pub dev_path: Option<String>,
}

/// Linux-specific extras for one device (matched by `id` against the common
/// manifest's device list).
#[derive(Debug, Clone, Deserialize)]
pub struct LinuxDeviceOverlay {
    pub id: String,
    /// GPIO character device this device's pins live on, e.g. `/dev/gpiochip0`
    /// (the default). The common manifest's `pins:` values are line offsets on
    /// this chip.
    #[serde(default)]
    pub gpio_chip: Option<String>,
    /// sysfs PWM chip a PWM output device's channel lives on, e.g. `pwmchip0`
    /// (the default). The common manifest's `pins.out` value is the channel
    /// index on this chip.
    #[serde(default)]
    pub pwm_chip: Option<String>,
}

/// Parse only the Linux extensions from a manifest YAML string.
pub fn parse_linux_overlay(yaml: &str) -> Result<LinuxManifestOverlay, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}
