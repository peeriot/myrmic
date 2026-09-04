//! Validation tests for linux-codegen (SR-3a, SR-3b, SR-4, SR-19).

#![allow(clippy::doc_markdown)]
//!
//! SR-3a: ESP32 example pipeline YAML passes with no `--manifest` (structural
//!        validation only).
//! SR-3b: Same YAML + reference Linux manifest generates successfully.
//! SR-4:  Manifest with a malformed `/dev/i2c-*` path fails with a named error.
//! SR-19: Pipeline with a non-empty `outlets:` list fails with
//!        "outlets are not yet supported on Linux".

use std::path::PathBuf;

use linux_codegen::backend::{validate_i2c_dev_path, validate_linux_manifest};
use linux_codegen::{generate_linux, validate_pipeline_only};
use pipeline_backend_api::manifest::parse_manifest;

/// Root of the repository, derived from CARGO_MANIFEST_DIR.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Cargo.toml lives at sdk/signal-layer/linux-codegen/
    // so we go up three levels to reach the repo root.
    manifest
        .ancestors()
        .nth(3)
        .expect("cannot locate repo root from CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn drivers_root() -> PathBuf {
    repo_root().join("signal-modules/drivers")
}

fn steps_root() -> PathBuf {
    repo_root().join("signal-modules/steps")
}

// ── SR-3a: structural validation only (no --manifest) ───────────────────────

/// SR-3a: The ESP32 sensors-only fixture YAML is a structurally valid pipeline
/// (it parses and passes structural validation when no manifest is provided).
#[test]
fn sr3a_esp32_sensors_only_passes_structural_validation() {
    let pipeline_path =
        repo_root().join("sdk/signal-layer/esp-codegen/tests/fixtures/sensors-only.yaml");
    let yaml = std::fs::read_to_string(&pipeline_path)
        .unwrap_or_else(|e| panic!("reading sensors-only.yaml: {e}"));
    let result = validate_pipeline_only(&yaml);
    assert!(
        result.is_ok(),
        "SR-3a: sensors-only YAML should pass structural validation, got: {:?}",
        result.unwrap_err()
    );
}

/// SR-3a: The Linux sensors-only fixture YAML also passes structural validation.
#[test]
fn sr3a_linux_sensors_only_passes_structural_validation() {
    let pipeline_path = fixtures_dir().join("linux-sensors-only.yaml");
    let yaml = std::fs::read_to_string(&pipeline_path).expect("reading linux-sensors-only.yaml");
    let result = validate_pipeline_only(&yaml);
    assert!(
        result.is_ok(),
        "SR-3a: linux-sensors-only.yaml should pass structural validation, got: {:?}",
        result.unwrap_err()
    );
}

// ── SR-3b: full generation with manifest ────────────────────────────────────

/// SR-3b: The Linux sensors-only fixture + reference manifest generates a crate.
#[test]
fn sr3b_linux_sensors_only_with_manifest_generates() {
    let pipeline_path = fixtures_dir().join("linux-sensors-only.yaml");
    let manifest_path = fixtures_dir().join("linux-manifest.yaml");

    let result = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    );
    assert!(
        result.is_ok(),
        "SR-3b: generation should succeed, got: {:#}",
        result.unwrap_err()
    );
    let crate_output = result.unwrap();
    assert_eq!(crate_output.pipeline_id, "linux-sensors");
    assert!(!crate_output.main_rs.is_empty());
    assert!(!crate_output.cargo_toml.is_empty());
    assert!(!crate_output.tap_contract_rs.is_empty());
}

// ── SR-4: malformed /dev/i2c-* path validation ──────────────────────────────

/// SR-4a: A well-formed `/dev/i2c-1` path passes path validation.
#[test]
fn sr4a_valid_i2c_dev_path_passes() {
    assert!(validate_i2c_dev_path("/dev/i2c-0").is_ok());
    assert!(validate_i2c_dev_path("/dev/i2c-1").is_ok());
    assert!(validate_i2c_dev_path("/dev/i2c-99").is_ok());
}

/// SR-4b: A malformed path fails with a named error.
#[test]
fn sr4b_malformed_i2c_dev_path_fails() {
    let result = validate_i2c_dev_path("/dev/i2c-BAD");
    assert!(result.is_err(), "expected error for malformed path, got Ok");
    let msg = result.unwrap_err();
    assert!(
        msg.contains("/dev/i2c-BAD"),
        "error should mention the bad path: {msg}"
    );
}

/// SR-4c: A path missing the `/dev/i2c-` prefix fails.
#[test]
fn sr4c_wrong_prefix_fails() {
    let result = validate_i2c_dev_path("/dev/spi-0");
    assert!(result.is_err());
    let msg = result.unwrap_err();
    assert!(msg.contains("expected `/dev/i2c-N`"), "got: {msg}");
}

/// SR-4d: A manifest with a missing/malformed /dev path fails generation.
#[test]
fn sr4d_manifest_with_bad_dev_path_fails_generation() {
    let pipeline_path = fixtures_dir().join("linux-sensors-only.yaml");
    let bad_manifest_path = fixtures_dir().join("bad-devpath-manifest.yaml");

    let result = generate_linux(
        &bad_manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    );
    assert!(
        result.is_err(),
        "expected generation to fail with malformed /dev path"
    );
    let err = format!("{:#}", result.unwrap_err());
    assert!(
        err.to_lowercase().contains("i2c") || err.contains("/dev/i2c-"),
        "error should mention the I2C path issue: {err}"
    );
}

/// SR-4e: validate_manifest rejects a manifest with a missing dev_path when
/// raw YAML is provided.
#[test]
fn sr4e_manifest_missing_dev_path_field_is_rejected() {
    let yaml = r"
id: rpi-no-path
chip: linux
buses:
  i2c0:
    transport: i2c
    pins: {}
    freq_khz: 400
gpios:
  general_purpose: []
devices: []
";
    let manifest = parse_manifest(yaml).expect("parse manifest");
    let errors = validate_linux_manifest(&manifest, Some(yaml));
    assert!(
        !errors.is_empty(),
        "expected errors for missing dev_path, got none"
    );
    assert!(
        errors.iter().any(|e| e.message.contains("dev_path")),
        "error should mention dev_path: {errors:?}"
    );
}

// ── SPI rejection ────────────────────────────────────────────────────────────

/// An SPI bus without a `dev_path` is rejected (no spidev naming convention
/// to fall back on).
#[test]
fn spi_bus_without_dev_path_is_rejected() {
    let yaml = std::fs::read_to_string(fixtures_dir().join("spi-manifest.yaml"))
        .expect("reading spi-manifest.yaml");
    let manifest = parse_manifest(&yaml).expect("parse manifest");
    let errors = validate_linux_manifest(&manifest, Some(&yaml));
    assert!(!errors.is_empty(), "expected dev_path rejection, got none");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("dev_path") && e.message.contains("spidev")),
        "error should require a spidev dev_path: {errors:?}"
    );
}

/// A complete SPI manifest (dev_path + cs pin) passes validation.
#[test]
fn spi_manifest_with_dev_path_passes() {
    let yaml = std::fs::read_to_string(fixtures_dir().join("linux-spi-manifest.yaml"))
        .expect("reading linux-spi-manifest.yaml");
    let manifest = parse_manifest(&yaml).expect("parse manifest");
    let errors = validate_linux_manifest(&manifest, Some(&yaml));
    assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
}

/// `/dev/spidevB.C` path validation accepts the well-formed shapes and
/// rejects everything else.
#[test]
fn spi_dev_path_format() {
    use linux_codegen::backend::validate_spi_dev_path;
    assert!(validate_spi_dev_path("/dev/spidev0.0").is_ok());
    assert!(validate_spi_dev_path("/dev/spidev10.2").is_ok());
    assert!(validate_spi_dev_path("/dev/spi0").is_err());
    assert!(validate_spi_dev_path("/dev/spidev0").is_err());
    assert!(validate_spi_dev_path("/dev/spidevA.B").is_err());
}

/// Full generation succeeds for the SPI loopback fixture.
#[test]
fn spi_loopback_generates() {
    let pipeline_path = fixtures_dir().join("linux-spi.yaml");
    let manifest_path = fixtures_dir().join("linux-spi-manifest.yaml");

    let result = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    );
    assert!(
        result.is_ok(),
        "SPI loopback generation failed: {:#}",
        result.unwrap_err()
    );
}

/// Manifest with an SPI bus lacking `dev_path` fails generation with a named
/// error.
///
/// Uses a no-source pipeline so the only failure path is the missing dev_path
/// (not a missing-device error from a pipeline that references I2C devices).
#[test]
fn spi_manifest_without_dev_path_fails_generation() {
    let spi_manifest_path = fixtures_dir().join("spi-manifest.yaml");

    // Write a minimal no-source pipeline inline to avoid the "missing device"
    // error that would mask the SPI rejection error.
    let pipeline_yaml = r"
pipeline:
  id: spi-test
sources: []
steps: []
taps: []
";
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    std::fs::write(tmp.path(), pipeline_yaml).expect("write pipeline");

    let result = generate_linux(
        &spi_manifest_path,
        tmp.path(),
        &drivers_root(),
        &steps_root(),
    );
    assert!(
        result.is_err(),
        "expected generation to fail for SPI manifest, got Ok"
    );
    let err = format!("{:#}", result.unwrap_err());
    assert!(
        err.to_lowercase().contains("spi"),
        "error should mention SPI: {err}"
    );
}

// ── SR-19 lifted: all outlet kinds accepted ──────────────────────────────────

/// SR-19 lifted: a pipeline with a cell-driven outlet (no `input:`) passes the
/// structural validation that used to reject it.
#[test]
fn cell_driven_outlet_is_accepted() {
    let pipeline_path = fixtures_dir().join("outlets-pipeline.yaml");
    let pipeline_yaml =
        std::fs::read_to_string(&pipeline_path).expect("reading outlets-pipeline.yaml");

    validate_pipeline_only(&pipeline_yaml)
        .expect("cell-driven outlets must pass structural validation");
}

/// A feed-forward outlet (with `input:`) passes structural validation.
#[test]
fn feed_forward_outlet_is_accepted() {
    let pipeline_path = fixtures_dir().join("linux-actuators.yaml");
    let pipeline_yaml =
        std::fs::read_to_string(&pipeline_path).expect("reading linux-actuators.yaml");

    validate_pipeline_only(&pipeline_yaml)
        .expect("feed-forward outlets must pass structural validation");
}

/// Full generation succeeds for the feed-forward actuator fixture.
#[test]
fn feed_forward_outlets_generate() {
    let pipeline_path = fixtures_dir().join("linux-actuators.yaml");
    let manifest_path = fixtures_dir().join("linux-actuators-manifest.yaml");

    let result = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    );
    assert!(
        result.is_ok(),
        "feed-forward actuator generation failed: {:#}",
        result.unwrap_err()
    );
}

// ── BLOCKER 1 (security): pipeline.id injection ──────────────────────────────

/// BLOCKER 1a: `validate_pipeline_only` rejects a pipeline.id with injection
/// characters (backtick/paren).  RED before the fix (was accepted).
#[test]
fn pipeline_id_with_backtick_is_rejected_by_validate_pipeline_only() {
    let yaml = r#"
pipeline:
  id: "x`);evil"
sources: []
steps: []
taps: []
"#;
    let result = validate_pipeline_only(yaml);
    assert!(
        result.is_err(),
        "BLOCKER 1: pipeline id with backtick/paren must be rejected, got Ok"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("pipeline id")
            || err.contains("identifier")
            || err.contains("illegal character"),
        "error should mention invalid pipeline id: {err}"
    );
}

/// BLOCKER 1b: `generate_linux` rejects a pipeline with an injected id.
#[test]
fn pipeline_id_with_injection_chars_is_rejected_by_generate_linux() {
    let pipeline_yaml = r#"
pipeline:
  id: "x\");std::process::Command::new(\"id\").status().unwrap();//"
sources: []
steps: []
taps: []
"#;
    // Write to a temp file since generate_linux takes a path.
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    std::fs::write(tmp.path(), pipeline_yaml).expect("write");

    let manifest_path = fixtures_dir().join("linux-manifest.yaml");
    let result = generate_linux(&manifest_path, tmp.path(), &drivers_root(), &steps_root());
    assert!(
        result.is_err(),
        "BLOCKER 1: pipeline id with injection characters must be rejected, got Ok"
    );
    let err = format!("{:#}", result.unwrap_err());
    assert!(
        err.contains("identifier") || err.contains("pipeline id") || err.contains("illegal"),
        "error should mention invalid pipeline id: {err}"
    );
}

/// Full generation succeeds for the mixed fixture (one cell-driven + one
/// feed-forward outlet).
#[test]
fn mixed_outlets_generate() {
    let pipeline_path = fixtures_dir().join("linux-outlets.yaml");
    let manifest_path = fixtures_dir().join("linux-actuators-manifest.yaml");

    let result = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    );
    assert!(
        result.is_ok(),
        "mixed outlet generation failed: {:#}",
        result.unwrap_err()
    );
}

// ── Device overlay: GPIO/PWM chip validation ─────────────────────────────────

/// A malformed `gpio_chip` (not `/dev/gpiochipN`) fails manifest validation.
#[test]
fn malformed_gpio_chip_is_rejected() {
    let yaml = std::fs::read_to_string(fixtures_dir().join("linux-actuators-manifest.yaml"))
        .expect("reading linux-actuators-manifest.yaml")
        .replace("gpio_chip: /dev/gpiochip0", "gpio_chip: /dev/gpio0");

    let (manifest, errors) = linux_codegen::backend::validate_linux_manifest_from_yaml(&yaml);
    manifest.expect("manifest should still parse");
    assert!(
        errors
            .iter()
            .any(|e| e.to_string().contains("invalid GPIO chip path")),
        "expected a GPIO chip path error, got: {errors:?}"
    );
}

/// A malformed `pwm_chip` (not `pwmchipN`) fails manifest validation.
#[test]
fn malformed_pwm_chip_is_rejected() {
    let yaml = std::fs::read_to_string(fixtures_dir().join("linux-actuators-manifest.yaml"))
        .expect("reading linux-actuators-manifest.yaml")
        .replace("pwm_chip: pwmchip0", "pwm_chip: /sys/class/pwm/pwmchip0");

    let (manifest, errors) = linux_codegen::backend::validate_linux_manifest_from_yaml(&yaml);
    manifest.expect("manifest should still parse");
    assert!(
        errors
            .iter()
            .any(|e| e.to_string().contains("invalid PWM chip name")),
        "expected a PWM chip name error, got: {errors:?}"
    );
}
