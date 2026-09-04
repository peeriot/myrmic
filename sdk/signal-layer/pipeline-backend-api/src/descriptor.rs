use indexmap::IndexMap;
use serde::Deserialize;

use crate::manifest::BusTransport;

/// One output field declared in a driver or step descriptor.
/// The `name` must match the exact Rust struct field name in `<Driver>Readings`.
#[derive(Debug, Clone, Deserialize)]
pub struct DriverOutput {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(default)]
    pub unit: String,
}

/// One input port declared in a step descriptor (the type a processing step consumes).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DriverInput {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
}

/// A bus a driver requires the board to provide (e.g. `transport: i2c`).
#[derive(Debug, Clone, Deserialize)]
pub struct RequiredBus {
    pub transport: BusTransport,
}

/// How an output driver drives its pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Digital on/off (a `DigitalState` toggles a GPIO high/low).
    Digital,
    /// PWM duty cycle (a `PwmDuty` sets a duty fraction).
    Pwm,
}

/// Output/actuator capability of a driver — the write-side mirror of `outputs`.
/// Present iff the driver drives an [`Outlet`]. The outlet bound to this device
/// (declared in the pipeline) supplies the command; the driver consumes it.
#[derive(Debug, Clone, Deserialize)]
pub struct DriverWrite {
    /// The command payload type the driver consumes (e.g. `DigitalState`,
    /// `PwmDuty`). Must equal the `type` of the pipeline outlet bound to this
    /// device.
    #[serde(rename = "type")]
    pub command_type: String,
    /// How the driver drives its output.
    pub mode: OutputMode,
}

/// Hardware capabilities a driver requires from the board.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Requires {
    #[serde(default)]
    pub buses: Vec<RequiredBus>,
    #[serde(default)]
    pub optional_pins: Vec<String>,
}

/// Configuration field scope.
///
/// - `hardware`: set in the board manifest device entry; describes physical
///   wiring (e.g. `i2c_addr`, `full_scale`). Must NOT appear in a pipeline.
/// - `application`: set in the pipeline source config; describes runtime
///   behaviour (e.g. `sample_interval_ms`). Must NOT appear in the board manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Hardware,
    #[default]
    Application,
}

/// A single entry in a driver or step descriptor's `configSchema`.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigField {
    #[serde(default)]
    pub scope: Scope,
    /// Rust type emitted in codegen (e.g. `u8`, `u32`, `f32`).
    pub rust_type: Option<String>,
    pub default: serde_yaml::Value,
}

/// Driver or step descriptor schema — holds config fields and output declarations.
/// Used for both drivers (`sensor-drivers`) and processing steps
/// (`processing-steps`); the YAML shape is identical for both categories.
/// Note: `requires.buses` is only meaningful for driver descriptors and is
/// silently ignored for steps.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DriverSchema {
    #[serde(default)]
    pub config_schema: IndexMap<String, ConfigField>,
    #[serde(default)]
    pub outputs: Vec<DriverOutput>,
    #[serde(default)]
    pub inputs: Vec<DriverInput>,
    #[serde(default)]
    pub requires: Requires,
    /// Output capability. `None` for a read-only (sensor) driver; `Some` for an
    /// output-capable driver that drives an Outlet.
    #[serde(default)]
    pub writes: Option<DriverWrite>,
}

/// Load a `DriverSchema` (or step schema) from a descriptor YAML string.
pub fn load_schema_from_yaml(yaml: &str) -> Result<DriverSchema, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}
