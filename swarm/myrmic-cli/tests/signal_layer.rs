//! `myrmic signal-layer`: the Signal Layer scaffolds, run through the built
//! binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The repository root as `--sdk`, so a scaffold resolves the peeriot crates
/// to this checkout.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs `myrmic <args> --registry-dir <registry>`, with `--sdk <repo root>`
/// for a scaffold that depends on the myrmic crates.
fn module(args: &[&str], registry: &Path, sdk: bool) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_myrmic"));
    cmd.args(args).arg("--registry-dir").arg(registry);
    if sdk {
        cmd.arg("--sdk").arg(repo_root());
    }
    cmd.output().expect("failed to run myrmic")
}

/// Runs `myrmic <args> <path> --sdk <repo root>`.
fn project(args: &[&str], path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .args(args)
        .arg(path)
        .arg("--sdk")
        .arg(repo_root())
        .output()
        .expect("failed to run myrmic")
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "myrmic failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn new_scaffolds_a_linux_pipeline_project() {
    let project_dir =
        tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let dir = project_dir.path().join("demo");

    assert_ok(&project(&["signal-layer", "new"], &dir));

    let manifest = std::fs::read_to_string(dir.join("board.yml")).expect("board.yml");
    assert!(
        manifest.contains("chip: linux"),
        "manifest targets linux:\n{manifest}"
    );
    assert!(
        manifest.contains("dev_path: /dev/i2c-1"),
        "manifest names the i2c dev path:\n{manifest}"
    );
    assert!(
        manifest.contains("driver: sim-source"),
        "manifest has the sim device:\n{manifest}"
    );

    let pipeline = std::fs::read_to_string(dir.join("pipeline.yml")).expect("pipeline.yml");
    assert!(
        pipeline.contains("device: sim"),
        "pipeline uses the sim source:\n{pipeline}"
    );

    let manifest_toml = std::fs::read_to_string(dir.join("Cargo.toml")).expect("Cargo.toml");
    for dep in [
        "signal-layer-linux-rt",
        "tokio",
        "embassy-time",
        "sim-source-driver",
        "linux-codegen",
    ] {
        assert!(
            manifest_toml.contains(dep),
            "Cargo.toml is missing `{dep}`:\n{manifest_toml}"
        );
    }

    let main = std::fs::read_to_string(dir.join("src/main.rs")).expect("main.rs");
    assert!(
        main.contains("#[tokio::main]"),
        "main is a tokio binary:\n{main}"
    );
    assert!(
        main.contains("mod pipeline_config"),
        "main includes the generated module:\n{main}"
    );
    assert!(
        main.contains("setup_tap_registry"),
        "main starts the tap registry:\n{main}"
    );
}

/// `--firmware` generates the pipeline into an ESP32 firmware image.
#[test]
fn new_firmware_scaffolds_the_pipeline_into_the_image() {
    let project_dir =
        tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let fw = project_dir.path().join("demo");

    assert_ok(&project(&["sl", "new", "--firmware=esp32c6"], &fw));

    let board = std::fs::read_to_string(fw.join("board.yml")).expect("board.yml is scaffolded");
    assert!(
        board.contains("chip: esp32c6"),
        "board.yml keeps the chip:\n{board}"
    );
    assert!(
        board.contains("driver: sim-source"),
        "board.yml has the sim device:\n{board}"
    );
    assert!(
        board.contains("general_purpose: ["),
        "board.yml lists usable pins:\n{board}"
    );

    let pipeline = std::fs::read_to_string(fw.join("pipeline.yml")).expect("pipeline.yml");
    assert!(
        pipeline.contains("device: sim"),
        "pipeline uses the sim source:\n{pipeline}"
    );
    assert!(
        pipeline.contains("kind: retained"),
        "pipeline exposes a tap:\n{pipeline}"
    );

    let manifest = std::fs::read_to_string(fw.join("Cargo.toml")).expect("Cargo.toml");
    assert!(
        manifest.contains("pipeline = ["),
        "a pipeline feature is present:\n{manifest}"
    );
    assert!(
        manifest.contains("sim-source-driver ="),
        "the sim-source driver is seeded:\n{manifest}"
    );
    assert!(
        manifest.contains("\"dep:sim-source-driver\""),
        "the pipeline feature enables the driver dep:\n{manifest}"
    );
    assert!(
        !manifest.lines().any(|line| line.starts_with("esp-hal =")),
        "a pipeline firmware needs no direct esp-hal dependency:\n{manifest}"
    );

    let main = std::fs::read_to_string(fw.join("src/main.rs")).expect("main.rs");
    assert!(
        main.contains("esp_firmware::pipeline!()"),
        "main pulls in the pipeline:\n{main}"
    );
    assert!(
        main.contains("pipeline_pins!"),
        "main claims the pipeline pins:\n{main}"
    );
    assert!(
        fw.join("partitions.toml").exists(),
        "a firmware scaffold carries the chip's partition layout"
    );
}

#[test]
fn driver_scaffolds_a_module_crate_under_the_registry() {
    let registry = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");

    assert_ok(&module(
        &["signal-layer", "driver", "demo_i2c", "--bus", "i2c"],
        registry.path(),
        false,
    ));

    let crate_dir = registry.path().join("drivers/demo_i2c");
    let manifest = std::fs::read_to_string(crate_dir.join("Cargo.toml")).expect("Cargo.toml");
    assert!(
        manifest
            .lines()
            .any(|line| line == r#"name = "demo_i2c-driver""#),
        "the driver crate is named after the id:\n{manifest}"
    );
    assert!(crate_dir.join("descriptor.yaml").exists());
    assert!(crate_dir.join("src/lib.rs").exists());
}

#[test]
fn driver_needs_a_bus() {
    let registry = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");

    let output = module(
        &["signal-layer", "driver", "demo_i2c"],
        registry.path(),
        false,
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--bus"), "{stderr}");
    assert!(!registry.path().join("drivers").exists());
}

/// `sl` is the short spelling of `signal-layer`.
#[test]
fn step_scaffolds_a_module_crate_under_the_registry() {
    let registry = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");

    assert_ok(&module(&["sl", "step", "demo_step"], registry.path(), true));

    let crate_dir = registry.path().join("steps/demo_step");
    let manifest = std::fs::read_to_string(crate_dir.join("Cargo.toml")).expect("Cargo.toml");
    assert!(
        manifest.lines().any(|line| line == r#"name = "demo_step""#),
        "{manifest}"
    );
    assert!(
        manifest.contains("signal-layer-core = { path = "),
        "the step depends on signal-layer-core from the checkout:\n{manifest}"
    );
    assert!(crate_dir.join("descriptor.yaml").exists());
}

#[test]
fn a_module_id_is_not_a_path() {
    let registry = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");

    let output = module(
        &["signal-layer", "step", "nested/demo_step"],
        registry.path(),
        true,
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("module id"), "{stderr}");
    assert!(!registry.path().join("steps").exists());
}

#[test]
fn new_seeds_the_lockfile_of_a_local_sdk() {
    let project_dir =
        tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let dir = project_dir.path().join("demo");

    assert_ok(&project(&["signal-layer", "new"], &dir));

    let sdk_lock = std::fs::read_to_string(repo_root().join("Cargo.lock")).expect("sdk lock");
    let seeded = std::fs::read_to_string(dir.join("Cargo.lock")).expect("seeded lock");
    assert_eq!(seeded, sdk_lock, "the sdk lockfile is copied as is");
}
