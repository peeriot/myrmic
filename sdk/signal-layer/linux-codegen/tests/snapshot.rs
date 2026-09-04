//! Snapshot test for linux-codegen (SR-7).
//!
//! Generates the sensors-only fixture (BME280 + VEML7700 + moving-average)
//! against the reference Linux manifest and asserts the output crate matches a
//! committed snapshot. Run with `UPDATE_GOLDEN=1` to (re)write the snapshot.

#![allow(clippy::doc_markdown)]
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo test -p linux-codegen --test snapshot
//! ```

use std::path::PathBuf;

use linux_codegen::generate_linux;

/// Root of the repository, derived from CARGO_MANIFEST_DIR.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
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

/// Run the snapshot test for a given fixture (base name without extension)
/// against a manifest fixture (base name without extension).
fn check_snapshot_with_manifest(fixture: &str, manifest: &str) {
    let pipeline_path = fixtures_dir().join(format!("{fixture}.yaml"));
    let manifest_path = fixtures_dir().join(format!("{manifest}.yaml"));
    let expected_main_rs_path = fixtures_dir().join(format!("{fixture}.expected.main.rs"));
    let expected_cargo_toml_path = fixtures_dir().join(format!("{fixture}.expected.Cargo.toml"));

    let crate_output = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    )
    .unwrap_or_else(|e| panic!("generation failed for fixture `{fixture}`: {e:#}"));

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&expected_main_rs_path, &crate_output.main_rs)
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", expected_main_rs_path.display()));
        std::fs::write(&expected_cargo_toml_path, &crate_output.cargo_toml).unwrap_or_else(|e| {
            panic!(
                "failed to write {}: {e}",
                expected_cargo_toml_path.display()
            )
        });
        eprintln!(
            "UPDATE_GOLDEN: wrote {} and {}",
            expected_main_rs_path.display(),
            expected_cargo_toml_path.display()
        );
        return;
    }

    let expected_main_rs = std::fs::read_to_string(&expected_main_rs_path).unwrap_or_else(|e| {
        panic!(
            "expected file {} not found — run with UPDATE_GOLDEN=1 first: {e}",
            expected_main_rs_path.display()
        )
    });
    let expected_cargo_toml =
        std::fs::read_to_string(&expected_cargo_toml_path).unwrap_or_else(|e| {
            panic!(
                "expected file {} not found — run with UPDATE_GOLDEN=1 first: {e}",
                expected_cargo_toml_path.display()
            )
        });

    if crate_output.main_rs != expected_main_rs {
        let actual_lines: Vec<&str> = crate_output.main_rs.lines().collect();
        let expected_lines: Vec<&str> = expected_main_rs.lines().collect();
        let first_diff = actual_lines
            .iter()
            .zip(expected_lines.iter())
            .enumerate()
            .find(|(_, (a, e))| a != e)
            .map_or_else(
                || {
                    format!(
                        "lengths differ: {} vs {}",
                        crate_output.main_rs.len(),
                        expected_main_rs.len()
                    )
                },
                |(i, (a, e))| format!("line {}: actual={a:?} expected={e:?}", i + 1),
            );
        panic!(
            "main.rs snapshot mismatch for fixture `{fixture}`.\n\
             First difference: {first_diff}\n\
             Re-run with UPDATE_GOLDEN=1 to refresh."
        );
    }

    assert_eq!(
        crate_output.cargo_toml, expected_cargo_toml,
        "Cargo.toml snapshot mismatch for fixture `{fixture}`.\n\
         Re-run with UPDATE_GOLDEN=1 to refresh."
    );
}

#[test]
fn snapshot_linux_sensors_only() {
    check_snapshot_with_manifest("linux-sensors-only", "linux-manifest");
}

/// Feed-forward actuators: a hysteresis-driven relay (GPIO line) and a
/// fan-curve-driven fan (sysfs PWM channel).
#[test]
fn snapshot_linux_actuators() {
    check_snapshot_with_manifest("linux-actuators", "linux-actuators-manifest");
}

/// Mixed outlets: one cell-driven (registry + sink task + IPC outlet store)
/// and one feed-forward (inline apply).
#[test]
fn snapshot_linux_outlets() {
    check_snapshot_with_manifest("linux-outlets", "linux-actuators-manifest");
}

/// SPI: spidev bus with software CS and the loopback source.
#[test]
fn snapshot_linux_spi() {
    check_snapshot_with_manifest("linux-spi", "linux-spi-manifest");
}

/// Assert the generated main.rs contains one tokio task per source with the
/// YAML's interval values.
#[test]
fn generated_main_rs_contains_tokio_tasks_with_correct_intervals() {
    let pipeline_path = fixtures_dir().join("linux-sensors-only.yaml");
    let manifest_path = fixtures_dir().join("linux-manifest.yaml");

    let crate_output = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    )
    .expect("generation should succeed");

    let src = &crate_output.main_rs;

    // One task per source.
    assert!(
        src.contains("bme280_task"),
        "missing bme280_task in generated main.rs"
    );
    assert!(
        src.contains("veml7700_task"),
        "missing veml7700_task in generated main.rs"
    );

    // tokio::main attribute.
    assert!(
        src.contains("tokio::main"),
        "missing #[tokio::main] in generated main.rs"
    );

    // tokio-based interval (from scaffold::tokio::emit_interval).
    assert!(
        src.contains("interval"),
        "missing interval() call in generated main.rs"
    );

    // BME280 interval: 1000 ms from the fixture.
    assert!(
        src.contains("1000"),
        "missing bme280 interval 1000ms in generated main.rs"
    );

    // VEML7700 interval: 2000 ms from the fixture.
    assert!(
        src.contains("2000"),
        "missing veml7700 interval 2000ms in generated main.rs"
    );

    // No embassy-specific tokens.
    assert!(
        !src.contains("embassy_executor"),
        "unexpected embassy_executor in Linux generated code"
    );
    assert!(
        !src.contains("StaticCell"),
        "unexpected StaticCell in Linux generated code"
    );
    assert!(
        !src.contains("NoopRawMutex"),
        "unexpected NoopRawMutex in Linux generated code"
    );

    // time seam from scaffold::tokio.
    assert!(
        src.contains("signal_layer_linux_rt"),
        "missing signal_layer_linux_rt time seam in generated main.rs"
    );

    // IPC server wiring (emit_tap_handoff).
    assert!(
        src.contains("run_signal_server") && src.contains("default_socket_path"),
        "missing IPC server setup in generated main.rs"
    );
}

/// Assert the generated Cargo.toml is a standalone project (has [workspace] table)
/// and includes critical-section/std.
#[test]
fn generated_cargo_toml_is_standalone_with_critical_section_std() {
    let pipeline_path = fixtures_dir().join("linux-sensors-only.yaml");
    let manifest_path = fixtures_dir().join("linux-manifest.yaml");

    let crate_output = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    )
    .expect("generation should succeed");

    let toml = &crate_output.cargo_toml;

    // Standalone project (has its own [workspace] table — D7).
    assert!(
        toml.contains("[workspace]"),
        "Cargo.toml must declare [workspace] to be standalone (D7)"
    );

    // critical-section/std (D7).
    assert!(
        toml.contains("critical-section") && toml.contains("std"),
        "Cargo.toml must include critical-section with std feature (D7)"
    );

    // tokio dependency.
    assert!(toml.contains("tokio"), "Cargo.toml must include tokio");

    // signal-layer-ipc and signal-layer-linux-rt.
    assert!(
        toml.contains("signal-layer-ipc"),
        "Cargo.toml must include signal-layer-ipc"
    );
    assert!(
        toml.contains("signal-layer-linux-rt"),
        "Cargo.toml must include signal-layer-linux-rt"
    );
}

/// Assert the generated tests/tap_contract.rs is present and non-empty.
#[test]
fn generated_tap_contract_rs_is_present() {
    let pipeline_path = fixtures_dir().join("linux-sensors-only.yaml");
    let manifest_path = fixtures_dir().join("linux-manifest.yaml");

    let crate_output = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    )
    .expect("generation should succeed");

    let tap_contract = &crate_output.tap_contract_rs;
    assert!(
        !tap_contract.is_empty(),
        "tap_contract.rs must not be empty"
    );
    assert!(
        tap_contract.contains("TapClient"),
        "tap_contract.rs must reference TapClient"
    );
    assert!(
        tap_contract.contains("temperature")
            || tap_contract.contains("humidity")
            || tap_contract.contains("lux"),
        "tap_contract.rs must reference the pipeline's tap names"
    );
}

/// Assert the generate output is byte-stable across repeated calls.
#[test]
fn generated_output_is_byte_stable() {
    let pipeline_path = fixtures_dir().join("linux-sensors-only.yaml");
    let manifest_path = fixtures_dir().join("linux-manifest.yaml");

    let a = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    )
    .expect("first generation");
    let b = generate_linux(
        &manifest_path,
        &pipeline_path,
        &drivers_root(),
        &steps_root(),
    )
    .expect("second generation");

    assert_eq!(
        a.main_rs, b.main_rs,
        "main.rs generation must be byte-stable"
    );
    assert_eq!(
        a.cargo_toml, b.cargo_toml,
        "Cargo.toml generation must be byte-stable"
    );
}
