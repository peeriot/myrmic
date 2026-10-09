//! Platform families: what a cell is built and deployed for (`myrmic build`/`deploy
//! --platform`). A family can cover several [`Platform`](crate::Platform)s, which name the chip a
//! runtime runs on: one riscv32imac artifact serves every ESP chip.

use std::str::FromStr;

use cell_protocol::ArtifactPlatform;

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub enum PlatformFamily {
    /// the host runtime (Wasmtime), running the cell's wasm directly
    Linux,
    /// Espressif riscv32imac boards (ESP32-C5, ESP32-C6, ESP32-C61), sharing one AOT artifact
    Riscv32imac,
}

impl PlatformFamily {
    pub const DEFAULT: &[Self] = &[Self::Linux];

    pub const ALL: &[Self] = &[Self::Linux, Self::Riscv32imac];

    /// Parses a `--platform` value: a comma-separated platform list, or
    /// [`PlatformFamily::DEFAULT`] when the flag was omitted.
    pub fn parse_list(spec: Option<&str>) -> anyhow::Result<Vec<Self>> {
        match spec {
            Some(spec) => spec.split(',').map(Self::from_str).collect(),
            None => Ok(Self::DEFAULT.to_vec()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Riscv32imac => ArtifactPlatform::Riscv32imac.as_str(),
        }
    }

    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Linux => &[],
            Self::Riscv32imac => &["esp32_c6"],
        }
    }
}

impl FromStr for PlatformFamily {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "linux" => Ok(Self::Linux),
            other => match other.parse::<ArtifactPlatform>() {
                Ok(ArtifactPlatform::Riscv32imac) => Ok(Self::Riscv32imac),
                Err(_) => anyhow::bail!("unknown platform: {}", other),
            },
        }
    }
}
