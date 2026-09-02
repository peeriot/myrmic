//! Generates the flash partition layout, the app partition table and the
//! main-stack and firmware-size link asserts. All the work lives in
//! `esp-firmware-build`, so a firmware crate's build script is one line.

fn main() {
    esp_firmware_build::configure();
}
