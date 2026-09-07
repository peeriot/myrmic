//! Generates the flash partition layout, the app partition table and the
//! main-stack and firmware-size link asserts. The heavy lifting lives in
//! `esp-firmware-build`; this script only picks the partition file.
//!
//! `modem-esp32` is one crate for three chips whose boards differ in flash, so
//! its layout can not be a single committed `partitions.toml`. Each layout lives
//! under `partitions/`; the file matching the chip being built is handed to
//! `esp-firmware-build`.

use std::path::PathBuf;

fn main() {
    let file = if std::env::var_os("CARGO_FEATURE_ESP32C5").is_some() {
        "esp32c5.toml"
    } else if std::env::var_os("CARGO_FEATURE_ESP32C61").is_some() {
        "esp32c61.toml"
    } else {
        // esp32c6 (the default chip) uses the 4 MB layout.
        "esp32c6.toml"
    };

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join("partitions").join(file);
    println!("cargo:rerun-if-changed={}", path.display());

    let partitions = esp_firmware_build::read_partitions_toml_at(&path)
        .unwrap_or_else(|e| panic!("{e}"))
        .unwrap_or_else(|| panic!("missing partition file {}", path.display()));

    esp_firmware_build::configure_with_partitions(&partitions);
}
