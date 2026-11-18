use indexmap::IndexMap;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardManifest {
    pub id: String,
    pub chip: String,
    pub buses: IndexMap<String, BusConfig>,
    pub gpios: GpioConfig,
    pub devices: Vec<DeviceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusConfig {
    pub transport: BusTransport,
    /// Pin role name → GPIO number (e.g. "scl" → 10, "sda" → 11).
    pub pins: IndexMap<String, u8>,
    pub freq_khz: u32,
    /// SPI mode (0–3, i.e. CPOL/CPHA). Ignored for I2C. Defaults to 0.
    #[serde(default)]
    pub mode: u8,
    /// Linux only: the device node backing the bus (`/dev/i2c-1`,
    /// `/dev/spidev0.0`). Other backends refuse it.
    pub dev_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusTransport {
    I2c,
    Spi,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpioConfig {
    pub general_purpose: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceEntry {
    pub id: String,
    pub driver: String,
    /// Bus id that this device is wired to (must be a key in `buses`). Empty for
    /// a bus-less device such as a GPIO/PWM actuator driven on a bare pin.
    #[serde(default)]
    pub bus: String,
    /// Named extra GPIO claims (e.g. drdy → GPIO4). Pin values must be in
    /// `gpios.general_purpose`.
    #[serde(default)]
    pub pins: IndexMap<String, u8>,
    /// Hardware-tier config fields (`i2c_addr`, `full_scale`, …).
    #[serde(default)]
    pub hardware: IndexMap<String, serde_yaml::Value>,
    /// Linux only: GPIO character device the device's pins live on, e.g.
    /// `/dev/gpiochip0` (the default). `pins:` values are line offsets on it.
    pub gpio_chip: Option<String>,
    /// Linux only: sysfs PWM chip a PWM output's channel lives on, e.g.
    /// `pwmchip0` (the default). `pins.out` is the channel index on it.
    pub pwm_chip: Option<String>,
}

pub fn parse_manifest(yaml: &str) -> Result<BoardManifest, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

#[cfg(test)]
mod tests {
    use super::parse_manifest;

    #[test]
    fn an_unknown_bus_key_is_rejected_by_name() {
        let err = parse_manifest(
            r"
id: b
chip: esp32c6
buses:
  spi2:
    transport: spi
    pins: {}
    freq_khz: 1000
    mod: 3
gpios:
  general_purpose: []
devices: []
",
        )
        .expect_err("a misspelled key must not parse");
        let msg = err.to_string();
        assert!(msg.contains("`mod`") && msg.contains("spi2"), "got: {msg}");
    }
}
