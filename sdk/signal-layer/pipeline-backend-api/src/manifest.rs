use indexmap::IndexMap;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct BoardManifest {
    pub id: String,
    pub chip: String,
    pub buses: IndexMap<String, BusConfig>,
    pub gpios: GpioConfig,
    pub devices: Vec<DeviceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BusConfig {
    pub transport: BusTransport,
    /// Pin role name → GPIO number (e.g. "scl" → 10, "sda" → 11).
    pub pins: IndexMap<String, u8>,
    pub freq_khz: u32,
    /// SPI mode (0–3, i.e. CPOL/CPHA). Ignored for I2C. Defaults to 0.
    #[serde(default)]
    pub mode: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusTransport {
    I2c,
    Spi,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GpioConfig {
    pub general_purpose: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
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
}

pub fn parse_manifest(yaml: &str) -> Result<BoardManifest, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}
