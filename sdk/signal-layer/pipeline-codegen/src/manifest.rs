//! Re-export shim: board manifest types now live in `pipeline-backend-api`.
//! This module re-exports the types and keeps the board-manifest validation
//! logic (which depends on `syn` and pipeline-codegen's validate module).

pub use pipeline_backend_api::manifest::{
    BoardManifest, BusConfig, BusTransport, DeviceEntry, GpioConfig, parse_manifest,
};

use crate::validate::ValidationError;
use indexmap::IndexMap;

/// Validate a board manifest without any pipeline or descriptor context.
/// Returns all errors found (does not stop at the first).
#[allow(clippy::too_many_lines)]
pub fn validate_manifest(manifest: &BoardManifest) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // All bus ids and device ids/drivers must be valid Rust identifiers.
    for bus_id in manifest.buses.keys() {
        if let Err(e) = crate::validate::validate_rust_ident(bus_id) {
            errors.push(ValidationError::new(format!("bus id {e}")));
        }
    }
    for device in &manifest.devices {
        if let Err(e) = crate::validate::validate_rust_ident(&device.id) {
            errors.push(ValidationError::new(format!("device id {e}")));
        }
        if let Err(e) = crate::validate::validate_rust_ident(&device.driver) {
            errors.push(ValidationError::new(format!(
                "device `{}`: driver {e}",
                device.id
            )));
        }
    }

    // Collect all pin numbers claimed by buses.
    let mut bus_pins: IndexMap<u8, String> = IndexMap::new();
    for (bus_id, bus) in &manifest.buses {
        for (role, &pin) in &bus.pins {
            if let Some(prev) = bus_pins.insert(pin, format!("{bus_id}.{role}")) {
                errors.push(ValidationError::new(format!(
                    "pin {pin} assigned to both `{prev}` and `{bus_id}.{role}`"
                )));
            }
        }
        if bus.freq_khz == 0 {
            errors.push(ValidationError::new(format!(
                "bus `{bus_id}`: freq_khz must be > 0"
            )));
        }
        if bus.transport == BusTransport::Spi {
            // Physical sclk/mosi pin wiring is chip-specific (spidev owns the
            // pins on Linux) and checked in the ESP backend's validate_manifest.
            if bus.mode > 3 {
                errors.push(ValidationError::new(format!(
                    "bus `{bus_id}`: SPI mode must be 0–3, got {}",
                    bus.mode
                )));
            }
        }
    }

    // Check no bus pin appears in general_purpose.
    let gp_set: std::collections::HashSet<u8> =
        manifest.gpios.general_purpose.iter().copied().collect();

    for (&pin, role) in &bus_pins {
        if gp_set.contains(&pin) {
            errors.push(ValidationError::new(format!(
                "pin {pin} is used as bus pin `{role}` but also listed in gpios.general_purpose"
            )));
        }
    }

    // For each device, validate:
    // 1. The bus id exists.
    // 2. Any `pins:` values are in general_purpose.
    for device in &manifest.devices {
        // An empty `bus` means a bus-less device (e.g. a GPIO/PWM actuator on a
        // bare pin); only a non-empty bus id must resolve to a declared bus.
        if !device.bus.is_empty() && !manifest.buses.contains_key(&device.bus) {
            errors.push(ValidationError::new(format!(
                "device `{}`: references unknown bus `{}`",
                device.id, device.bus
            )));
        }

        for (pin_name, &pin) in &device.pins {
            if !gp_set.contains(&pin) {
                errors.push(ValidationError::new(format!(
                    "device `{}`: pin `{pin_name}` = GPIO{pin} is not in gpios.general_purpose",
                    device.id
                )));
            }
        }

        if let Some(bus) = manifest.buses.get(&device.bus)
            && bus.transport == BusTransport::Spi
            && !device.pins.contains_key("cs")
        {
            errors.push(ValidationError::new(format!(
                "device `{}`: SPI device must declare a `cs` pin",
                device.id
            )));
        }
    }

    // Static pin mutual-exclusion (E1/OUT-11): every GPIO pin is owned by at
    // most one device.
    let mut pin_owner: IndexMap<u8, String> = IndexMap::new();
    for device in &manifest.devices {
        for (pin_name, &pin) in &device.pins {
            let owner = format!("{}.{pin_name}", device.id);
            if let Some(prev) = pin_owner.insert(pin, owner.clone()) {
                errors.push(ValidationError::new(format!(
                    "GPIO{pin} is claimed by both `{prev}` and `{owner}`"
                )));
            }
        }
    }

    // Check for duplicate device ids.
    let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for device in &manifest.devices {
        if !seen_ids.insert(device.id.as_str()) {
            errors.push(ValidationError::new(format!(
                "duplicate device id `{}`",
                device.id
            )));
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_MANIFEST: &str = r"
id: test-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins:
      scl: 10
      sda: 11
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5, 6, 7, 12, 13]
devices:
  - id: bme280
    driver: bme280
    bus: i2c0
    hardware:
      i2c_addr: 0x76
";

    #[test]
    fn valid_manifest_parses_and_validates() {
        let m = parse_manifest(VALID_MANIFEST).unwrap();
        let errors = validate_manifest(&m);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(m.id, "test-board");
        assert_eq!(m.buses["i2c0"].transport, BusTransport::I2c);
        assert_eq!(m.buses["i2c0"].freq_khz, 400);
        assert_eq!(m.devices[0].id, "bme280");
    }

    #[test]
    fn bus_pin_in_general_purpose_is_rejected() {
        let yaml = r"
id: bad-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins:
      scl: 10
      sda: 11
    freq_khz: 400
gpios:
  general_purpose: [10, 11, 12, 13]
devices: []
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        assert!(
            errors.len() >= 2,
            "expected at least 2 pin-conflict errors, got: {errors:?}"
        );
        assert!(errors.iter().any(|e| e.message.contains("pin 10")));
        assert!(errors.iter().any(|e| e.message.contains("pin 11")));
    }

    #[test]
    fn device_with_unknown_bus_is_rejected() {
        let yaml = r"
id: bad-board
chip: esp32c6
buses: {}
gpios:
  general_purpose: [0, 1, 2]
devices:
  - id: sensor
    driver: bme280
    bus: i2c_does_not_exist
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        assert!(errors.iter().any(|e| e.message.contains("unknown bus")));
    }

    #[test]
    fn device_pin_not_in_general_purpose_is_rejected() {
        let yaml = r"
id: bad-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins:
      scl: 10
      sda: 11
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2]
devices:
  - id: sensor
    driver: bme280
    bus: i2c0
    pins:
      drdy: 5
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("GPIO5") || e.message.contains("drdy")),
            "expected pin error, got: {errors:?}"
        );
    }

    #[test]
    fn devkit_manifest_parses_and_validates() {
        let yaml =
            include_str!("../../../../embedded/esp-hal/signal-layer/boards/esp32c6-devkit.yaml");
        let m = parse_manifest(yaml).expect("devkit manifest should parse");
        let errors = validate_manifest(&m);
        assert!(
            errors.is_empty(),
            "devkit manifest validation errors: {errors:?}"
        );
        assert_eq!(m.id, "esp32c6-devkit");
        assert_eq!(m.chip, "esp32c6");
        assert!(m.buses.contains_key("i2c0"));
        assert_eq!(m.buses["i2c0"].pins["scl"], 10);
        assert_eq!(m.buses["i2c0"].pins["sda"], 11);
        assert!(!m.devices.is_empty());
    }

    #[test]
    fn duplicate_device_ids_are_rejected() {
        let yaml = r"
id: bad-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins:
      scl: 10
      sda: 11
    freq_khz: 400
gpios:
  general_purpose: [0, 1]
devices:
  - id: sensor
    driver: bme280
    bus: i2c0
  - id: sensor
    driver: bmp180
    bus: i2c0
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("duplicate device id"))
        );
    }

    #[test]
    fn digit_leading_bus_id_is_rejected() {
        let yaml = r"
id: bad-board
chip: esp32c6
buses:
  7bus:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1]
devices: []
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("bus id") && e.message.contains("7bus")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn digit_leading_device_id_is_rejected() {
        let yaml = r"
id: bad-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1]
devices:
  - id: 3sensor
    driver: bme280
    bus: i2c0
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("device id") && e.message.contains("3sensor")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn digit_leading_driver_id_is_rejected() {
        let yaml = r"
id: bad-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1]
devices:
  - id: sensor
    driver: 7segment
    bus: i2c0
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("driver") && e.message.contains("7segment")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn busless_output_device_validates() {
        let yaml = r"
id: test-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5]
devices:
  - id: relay1
    driver: gpio-output
    pins:
      out: 5
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(m.devices[0].bus, "");
    }

    #[test]
    fn two_devices_claiming_one_pin_is_rejected() {
        let yaml = r"
id: bad-board
chip: esp32c6
buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1, 2, 3, 4, 5]
devices:
  - id: relay1
    driver: gpio-output
    pins: { out: 5 }
  - id: relay2
    driver: gpio-output
    pins: { out: 5 }
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("GPIO5") && e.message.contains("claimed by both")),
            "got: {errors:?}"
        );
    }

    #[test]
    fn hyphenated_bus_and_device_ids_pass_validation() {
        let yaml = r"
id: good-board
chip: esp32c6
buses:
  i2c-main:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
gpios:
  general_purpose: [0, 1]
devices:
  - id: my-sensor
    driver: my-driver
    bus: i2c-main
";
        let m = parse_manifest(yaml).unwrap();
        let errors = validate_manifest(&m);
        let ident_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.message.contains("identifier"))
            .collect();
        assert!(
            ident_errors.is_empty(),
            "hyphenated ids should be valid: {ident_errors:?}"
        );
    }
}
