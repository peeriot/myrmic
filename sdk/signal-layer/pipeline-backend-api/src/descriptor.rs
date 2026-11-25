use std::fmt::{Formatter, Result as FmtResult};

use indexmap::IndexMap;
use serde::de::value::MapAccessDeserializer;
use serde::de::{Error as DeError, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

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
/// Present iff the driver drives an `Outlet`. The outlet bound to this device
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
    pub optional_pins: Vec<OptionalPin>,
}

impl Requires {
    /// Direction the driver declares for the optional pin `name`; a pin the
    /// descriptor does not list is an input.
    #[must_use]
    pub fn pin_direction(&self, name: &str) -> PinDirection {
        self.optional_pins
            .iter()
            .find(|pin| pin.name == name)
            .map_or_else(PinDirection::default, |pin| pin.direction)
    }
}

/// Direction of an optional device pin, as seen from the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PinDirection {
    /// The driver reads the pin (e.g. an interrupt line).
    #[default]
    Input,
    /// The driver drives the pin (e.g. a reset line).
    Output,
}

/// An optional device pin declared by a driver descriptor.
///
/// Written either as a bare name (`nint`, direction `input`) or as a map
/// (`{ name: reset, direction: output }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionalPin {
    /// Pin name, as used in the manifest device `pins` map.
    pub name: String,
    /// Direction as seen from the driver.
    pub direction: PinDirection,
}

impl OptionalPin {
    /// An input pin called `name`.
    pub fn input(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            direction: PinDirection::Input,
        }
    }
}

impl<'de> Deserialize<'de> for OptionalPin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PinVisitor;

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Full {
            name: String,
            #[serde(default)]
            direction: PinDirection,
        }

        impl<'de> Visitor<'de> for PinVisitor {
            type Value = OptionalPin;

            fn expecting(&self, f: &mut Formatter<'_>) -> FmtResult {
                f.write_str("a pin name or a `{ name, direction }` map")
            }

            fn visit_str<E>(self, name: &str) -> Result<OptionalPin, E>
            where
                E: DeError,
            {
                Ok(OptionalPin::input(name))
            }

            fn visit_map<A>(self, map: A) -> Result<OptionalPin, A::Error>
            where
                A: MapAccess<'de>,
            {
                let Full { name, direction } = Full::deserialize(MapAccessDeserializer::new(map))?;

                Ok(OptionalPin { name, direction })
            }
        }

        deserializer.deserialize_any(PinVisitor)
    }
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
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConfigField {
    #[serde(default)]
    pub scope: Scope,
    /// Rust type emitted in codegen (e.g. `u8`, `u32`, `f32`).
    pub rust_type: Option<String>,
    pub default: serde_yaml::Value,
    /// Inclusive bounds for a numeric field; a value outside fails validation.
    pub min: Option<serde_yaml::Value>,
    pub max: Option<serde_yaml::Value>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_string_is_an_input_pin() {
        assert_eq!(
            pins("requires: { optional_pins: [nint] }"),
            [OptionalPin::input("nint")]
        );
    }

    #[test]
    fn map_form_carries_the_direction() {
        let parsed = pins("requires: { optional_pins: [{ name: reset, direction: output }, irq] }");

        assert_eq!(parsed[0].name, "reset");
        assert_eq!(parsed[0].direction, PinDirection::Output);
        assert_eq!(parsed[1].name, "irq");
        assert_eq!(parsed[1].direction, PinDirection::Input);
    }

    #[test]
    fn map_without_direction_defaults_to_input() {
        let parsed = pins("requires: { optional_pins: [{ name: irq }] }");

        assert_eq!(parsed[0].direction, PinDirection::Input);
    }

    #[test]
    fn unknown_direction_is_a_named_error() {
        let err = load_schema_from_yaml(
            "requires: { optional_pins: [{ name: x, direction: sideways }] }",
        )
        .expect_err("bad direction must fail");

        assert!(err.to_string().contains("sideways"), "{err}");
    }

    #[test]
    fn map_without_name_is_rejected() {
        let err = load_schema_from_yaml("requires: { optional_pins: [{ direction: input }] }")
            .expect_err("name is required");

        assert!(err.to_string().contains("missing field `name`"), "{err}");
    }

    fn pins(yaml: &str) -> Vec<OptionalPin> {
        load_schema_from_yaml(yaml)
            .expect("descriptor parses")
            .requires
            .optional_pins
    }
}
