//! Generates the flash partition layout, the app partition table and the
//! main-stack and firmware-size link asserts, and — when the `pipeline` feature
//! is enabled — the Signal Layer pipeline. The heavy lifting lives in
//! `esp-firmware-build`.
//!
//! `modem-esp32` is one crate for three chips whose boards differ in flash, so
//! its partition layout can not be a single committed `partitions.toml`. Each
//! layout lives under `partitions/`; the file matching the chip being built is
//! handed to `esp-firmware-build`.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let path = manifest_dir
        .join("partitions")
        .join(format!("{}.toml", chip()));
    println!("cargo:rerun-if-changed={}", path.display());

    let partitions = esp_firmware_build::read_partitions_toml_at(&path)
        .unwrap_or_else(|e| panic!("{e}"))
        .unwrap_or_else(|| panic!("missing partition file {}", path.display()));

    esp_firmware_build::configure_with_partitions(&partitions);

    // The pipeline is generated only when the `pipeline` feature is on; without
    // it there is nothing to generate and the firmware builds pipeline-free.
    if std::env::var_os("CARGO_FEATURE_PIPELINE").is_some() {
        generate_pipeline();
    }
}

/// Generate the Signal Layer pipeline into `$OUT_DIR`, where the `pipeline!()`
/// macro in `main.rs` includes it.
///
/// The board manifest and pipeline YAML are chosen by the `SIGNAL_LAYER_BOARD`
/// and `SIGNAL_LAYER_PIPELINE` env vars, defaulting to this chip's devkit board
/// and the `basic-sensors` starter. HIL and bench builds set them to a scenario
/// (e.g. `SIGNAL_LAYER_PIPELINE=…/hil-tests.yaml`), which is why this crate is
/// one firmware for every pipeline rather than one crate per pipeline.
fn generate_pipeline() {
    // The shared Signal Layer manifests, relative to this crate root.
    const SIGNAL_LAYER: &str = "../signal-layer";

    println!("cargo:rerun-if-env-changed=SIGNAL_LAYER_BOARD");
    println!("cargo:rerun-if-env-changed=SIGNAL_LAYER_PIPELINE");

    let board = std::env::var("SIGNAL_LAYER_BOARD")
        .unwrap_or_else(|_| format!("{SIGNAL_LAYER}/boards/{}-devkit.yaml", chip()));
    let pipeline = std::env::var("SIGNAL_LAYER_PIPELINE")
        .unwrap_or_else(|_| format!("{SIGNAL_LAYER}/pipelines/basic-sensors.yaml"));

    esp_firmware_build::pipeline()
        .board(board)
        .pipeline(pipeline)
        .generate();
}

/// The target chip, from the `esp32c*` feature cargo sets. Defaults to the
/// 4 MB esp32c6.
fn chip() -> &'static str {
    if std::env::var_os("CARGO_FEATURE_ESP32C5").is_some() {
        "esp32c5"
    } else if std::env::var_os("CARGO_FEATURE_ESP32C61").is_some() {
        "esp32c61"
    } else {
        "esp32c6"
    }
}
