//! Building a firmware crate — an `esp-firmware` binary — for its chip.
//!
//! A firmware crate names its chip in `[package.metadata.myrmic] firmware`,
//! which is what tells it apart from a cell crate. It compiles in release mode
//! for the chip's bare-metal RISC-V target with the link flags esp-hal needs,
//! and hands `esp-firmware-build` a default partition layout when the crate
//! ships no `partitions.toml` of its own.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use esp_firmware_build::{PARTITIONS_ENV, read_partitions_toml};
pub use esp_firmware_build::{Partitions, spell_size};
use serde::Deserialize;

use crate::CargoTarget;
use crate::cargo;
use crate::compile::{Selector, TOOLCHAIN, resolve_selector};

/// The bare-metal target every supported chip compiles for.
pub const TARGET: &str = "riscv32imac-unknown-none-elf";
const RUSTFLAGS_ENV: &str = "CARGO_TARGET_RISCV32IMAC_UNKNOWN_NONE_ELF_RUSTFLAGS";
/// esp-hal's linker script, frame pointers for its backtraces, and lld.
const RUSTFLAGS: &str = "-C link-arg=-Tlinkall.x -C force-frame-pointers -C linker=rust-lld";
/// Compile-time configuration the firmware expects. A value already in the
/// caller's environment wins, so `ESP_LOG=debug myrmic build` works.
const DEFAULT_ENV: [(&str, &str); 2] = [
    ("ESP_LOG", "info"),
    ("ESP_HAL_CONFIG_ENSURE_MAIN_STACK_MINIMUM", "4096"),
];
/// The name the device registers with, baked into the image at compile time;
/// without it the firmware falls back to its chip's name.
pub const RUNTIME_NAME_ENV: &str = "RUNTIME_NAME";

/// A supported target SoC, spelled as its cargo feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Chip {
    Esp32c5,
    Esp32c6,
    Esp32c61,
}

impl Chip {
    pub const ALL: [Self; 3] = [Self::Esp32c5, Self::Esp32c6, Self::Esp32c61];

    pub fn name(self) -> &'static str {
        match self {
            Self::Esp32c5 => "esp32c5",
            Self::Esp32c6 => "esp32c6",
            Self::Esp32c61 => "esp32c61",
        }
    }
}

impl std::fmt::Display for Chip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Chip {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|chip| chip.name() == s.trim())
            .with_context(|| {
                let names: Vec<_> = Self::ALL.iter().map(|chip| chip.name()).collect();
                format!("unknown chip `{s}`; expected one of {}", names.join(", "))
            })
    }
}

/// The `[package.metadata.myrmic]` keys a firmware crate sets.
#[derive(Deserialize, Default)]
struct Metadata {
    firmware: Option<Chip>,
}

/// The chip named by `[package.metadata.myrmic] firmware`, or `None` for a
/// cell crate (or a manifest with no `[package]` at all).
pub fn chip_of(manifest_path: &Path) -> anyhow::Result<Option<Chip>> {
    Ok(cargo::read_package_metadata::<Metadata>(manifest_path)?.and_then(|m| m.firmware))
}

/// What a firmware build produced.
pub struct FirmwareBuild {
    /// The linked ELF, ready for `espflash flash`.
    pub elf: PathBuf,
    /// The app partition table `esp-firmware-build` generated for the layout,
    /// as recorded in the crate's `espflash.toml`.
    pub partition_table: Option<PathBuf>,
    /// The layout supplied in place of a `partitions.toml`; `None` when the
    /// crate has its own.
    pub default_partitions: Option<Partitions>,
}

/// Builds the firmware crate at `manifest_path` in release mode for [`TARGET`],
/// compiling the binary `cargo_target` selects.
///
/// `flash_size` is the attached board's flash in bytes, when a board is at
/// hand: it sizes the default partition layout, and a `partitions.toml` that
/// claims more flash than that is rejected. `runtime_name` names the device on
/// the network; given, it overrides a [`RUNTIME_NAME_ENV`] in the environment.
pub fn build(
    manifest_path: &Path,
    cargo_target: &CargoTarget,
    flash_size: Option<u64>,
    runtime_name: Option<&str>,
) -> anyhow::Result<FirmwareBuild> {
    let manifest_dir = manifest_path.parent().with_context(|| {
        format!(
            "manifest has no parent directory: {}",
            manifest_path.display()
        )
    })?;

    let bin = match resolve_selector(manifest_path, cargo_target)? {
        Selector::Bin(name) => name,
        Selector::Lib => anyhow::bail!("a firmware is a binary; `lib` is not a firmware target"),
    };

    let mut cmd = Command::new("cargo");
    cmd.current_dir(manifest_dir);
    cmd.arg(format!("+{TOOLCHAIN}"));
    cmd.args([
        "build",
        "--release",
        "--target",
        TARGET,
        "--manifest-path",
    ])
    .arg(manifest_path)
    .args(["--bin", &bin]);
    let default_partitions = configure(&mut cmd, manifest_dir, flash_size, runtime_name, |key| {
        std::env::var_os(key).is_some()
    })?;
    cmd.env_remove("RUSTUP_TOOLCHAIN");

    let mut elf = None;
    cargo::process_cargo_build(cmd, |artifact| {
        if artifact.executable {
            elf = Some(artifact.path.clone());
        }
    })?;
    let elf = elf.context("firmware build produced no executable")?;

    Ok(FirmwareBuild {
        elf,
        partition_table: partition_table(manifest_dir),
        default_partitions,
    })
}

/// Sets the target's link flags, the compile-time environment (only the keys
/// `already_set` says the caller hasn't), the runtime name when one is given,
/// and — for a crate without a `partitions.toml` — the default partition
/// layout for `flash_size`, which is returned.
fn configure(
    cmd: &mut Command,
    manifest_dir: &Path,
    flash_size: Option<u64>,
    runtime_name: Option<&str>,
    already_set: impl Fn(&str) -> bool,
) -> anyhow::Result<Option<Partitions>> {
    cmd.env(RUSTFLAGS_ENV, RUSTFLAGS);
    for (key, value) in DEFAULT_ENV {
        if !already_set(key) {
            cmd.env(key, value);
        }
    }
    if let Some(name) = runtime_name {
        cmd.env(RUNTIME_NAME_ENV, name);
    }

    if let Some(file) = read_partitions_toml(manifest_dir).map_err(anyhow::Error::msg)? {
        if let (Some(claimed), Some(device)) = (file.flash_size, flash_size)
            && claimed > device
        {
            anyhow::bail!(
                "partitions.toml sets flash_size = \"{}\", but the board has {} of flash",
                spell_size(claimed),
                spell_size(device)
            );
        }
        return Ok(None);
    }
    let partitions = Partitions::default_layout(flash_size);
    cmd.env(PARTITIONS_ENV, partitions.to_compact());
    Ok(Some(partitions))
}

#[derive(Deserialize)]
struct EspflashConfig {
    idf_format_args: Option<IdfFormatArgs>,
}

#[derive(Deserialize)]
struct IdfFormatArgs {
    partition_table: Option<PathBuf>,
}

/// The partition table the crate's `espflash.toml` points at, if it has one.
fn partition_table(manifest_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(manifest_dir.join("espflash.toml")).ok()?;
    let config: EspflashConfig = toml::from_str(&text).ok()?;
    config.idf_format_args?.partition_table
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn manifest_with(metadata: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        std::fs::write(
            &path,
            format!("[package]\nname = \"fw\"\nversion = \"0.1.0\"\n{metadata}"),
        )
        .unwrap();
        (dir, path)
    }

    fn env_of(cmd: &Command, key: &str) -> Option<String> {
        cmd.get_envs()
            .find(|(k, _)| *k == key)
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    #[test]
    fn chip_of_reads_the_firmware_chip_from_myrmic_metadata() {
        let (_dir, path) = manifest_with("[package.metadata.myrmic]\nfirmware = \"esp32c6\"\n");
        assert_eq!(chip_of(&path).unwrap(), Some(Chip::Esp32c6));
    }

    #[test]
    fn chip_of_is_none_for_a_cell_crate() {
        let (_dir, path) = manifest_with("[package.metadata.myrmic]\nheap_size = 2048\n");
        assert_eq!(chip_of(&path).unwrap(), None);
        let (_dir, path) = manifest_with("");
        assert_eq!(chip_of(&path).unwrap(), None);
    }

    #[test]
    fn chip_of_rejects_an_unknown_chip() {
        let (_dir, path) = manifest_with("[package.metadata.myrmic]\nfirmware = \"esp32\"\n");
        let err = format!("{:#}", chip_of(&path).unwrap_err());
        assert!(err.contains("esp32"), "{err}");
    }

    #[test]
    fn chip_parses_and_prints_its_feature_name() {
        assert_eq!("esp32c61".parse::<Chip>().unwrap(), Chip::Esp32c61);
        assert_eq!(Chip::Esp32c5.to_string(), "esp32c5");
        assert!("esp32c3".parse::<Chip>().is_err());
    }

    const M: u64 = 1024 * 1024;

    #[test]
    fn configure_links_with_the_esp_flags_for_the_riscv_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("cargo");
        configure(&mut cmd, dir.path(), None, None, |_| false).unwrap();

        let rustflags = env_of(&cmd, RUSTFLAGS_ENV).expect("rustflags are set");
        for flag in [
            "link-arg=-Tlinkall.x",
            "force-frame-pointers",
            "linker=rust-lld",
        ] {
            assert!(rustflags.contains(flag), "{rustflags}");
        }
        assert_eq!(env_of(&cmd, "ESP_LOG").as_deref(), Some("info"));
        assert_eq!(
            env_of(&cmd, "ESP_HAL_CONFIG_ENSURE_MAIN_STACK_MINIMUM").as_deref(),
            Some("4096")
        );
    }

    #[test]
    fn configure_leaves_env_the_caller_already_set_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("cargo");
        configure(&mut cmd, dir.path(), None, None, |key| key == "ESP_LOG").unwrap();

        assert_eq!(env_of(&cmd, "ESP_LOG"), None);
        assert_eq!(
            env_of(&cmd, "ESP_HAL_CONFIG_ENSURE_MAIN_STACK_MINIMUM").as_deref(),
            Some("4096")
        );
    }

    #[test]
    fn configure_bakes_in_the_runtime_name_it_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("cargo");
        configure(&mut cmd, dir.path(), None, Some("kitchen"), |_| false).unwrap();

        assert_eq!(env_of(&cmd, RUNTIME_NAME_ENV).as_deref(), Some("kitchen"));
    }

    #[test]
    fn configure_leaves_the_runtime_name_to_the_environment_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("cargo");
        configure(&mut cmd, dir.path(), None, None, |_| false).unwrap();

        assert_eq!(env_of(&cmd, RUNTIME_NAME_ENV), None);
    }

    #[test]
    fn an_explicit_runtime_name_overrides_one_already_in_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("cargo");
        configure(&mut cmd, dir.path(), None, Some("kitchen"), |key| {
            key == RUNTIME_NAME_ENV
        })
        .unwrap();

        assert_eq!(env_of(&cmd, RUNTIME_NAME_ENV).as_deref(), Some("kitchen"));
    }

    #[test]
    fn configure_supplies_the_default_partitions_only_without_a_partitions_toml() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("cargo");
        let supplied = configure(&mut cmd, dir.path(), None, None, |_| false).unwrap();
        assert_eq!(supplied, Some(Partitions::default_layout(None)));
        assert_eq!(
            env_of(&cmd, PARTITIONS_ENV),
            Some(Partitions::default_layout(None).to_compact())
        );

        std::fs::write(dir.path().join("partitions.toml"), "[partitions]\n").unwrap();
        let mut cmd = Command::new("cargo");
        assert_eq!(
            configure(&mut cmd, dir.path(), None, None, |_| false).unwrap(),
            None
        );
        assert_eq!(env_of(&cmd, PARTITIONS_ENV), None);
    }

    #[test]
    fn configure_sizes_the_default_layout_to_the_device_flash() {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new("cargo");
        let supplied = configure(&mut cmd, dir.path(), Some(8 * M), None, |_| false).unwrap();
        assert_eq!(supplied, Some(Partitions::default_layout(Some(8 * M))));
        assert_eq!(
            env_of(&cmd, PARTITIONS_ENV),
            Some(Partitions::default_layout(Some(8 * M)).to_compact())
        );
    }

    #[test]
    fn configure_rejects_a_partitions_toml_that_claims_more_flash_than_the_device_has() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("partitions.toml"),
            "[partitions]\nflash_size = \"8M\"\n",
        )
        .unwrap();

        let mut cmd = Command::new("cargo");
        let err = configure(&mut cmd, dir.path(), Some(4 * M), None, |_| false).unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("8M") && err.contains("4M"), "{err}");

        // The same file is fine on a board that has the flash it claims, or
        // when no device size is known (`myrmic build`).
        let mut cmd = Command::new("cargo");
        assert_eq!(
            configure(&mut cmd, dir.path(), Some(8 * M), None, |_| false).unwrap(),
            None
        );
        let mut cmd = Command::new("cargo");
        assert_eq!(
            configure(&mut cmd, dir.path(), None, None, |_| false).unwrap(),
            None
        );
    }

    #[test]
    fn partition_table_is_read_from_the_generated_espflash_toml() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(partition_table(dir.path()), None);

        std::fs::write(
            dir.path().join("espflash.toml"),
            "[idf_format_args]\npartition_table = \"/out/partitions.generated.csv\"\n",
        )
        .unwrap();
        assert_eq!(
            partition_table(dir.path()),
            Some(std::path::PathBuf::from("/out/partitions.generated.csv"))
        );
    }
}
