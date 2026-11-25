//! `myrmic signal-layer new`: a pipeline project, standalone on Linux or
//! generated into an ESP32 firmware image.

use myrmic_build::firmware::Chip;

use super::pins::{BoardPins, board_pins};
use crate::args::Ctx;
use crate::cmd::new::{render_project, seed_lockfile, write_partitions};
use crate::utils::validate_name;
use crate::{determine_name, models};

#[derive(textus::Template)]
#[template(path = "templates/firmware-pipeline", strip_suffix = ".tmpl")]
struct TemplateNewFirmwarePipeline<'a> {
    name: &'a str,
    chip: &'a str,
    scl: u8,
    sda: u8,
    general_purpose: String,
    firmware_sdk: models::CargoDep,
    firmware_build: models::CargoDep,
    module_deps: models::CargoDeps,
    pipeline_feature_deps: models::DepFeatures,
    toolchain: &'a str,
}

#[derive(textus::Template)]
#[template(path = "templates/linux-pipeline", strip_suffix = ".tmpl")]
struct TemplateNewLinuxPipeline<'a> {
    name: &'a str,
    signal_layer_ipc: models::CargoDep,
    signal_layer_linux_rt: models::CargoDep,
    linux_i2c_shim: models::CargoDep,
    linux_gpio_shim: models::CargoDep,
    linux_spi_shim: models::CargoDep,
    signal_layer_core: models::CargoDep,
    signal_layer_types: models::CargoDep,
    module_deps: models::CargoDeps,
    linux_codegen: models::CargoDep,
}

#[derive(clap::Parser)]
pub struct New {
    /// Directory to create the project in.
    path: std::path::PathBuf,

    /// Crate name (default: the last component of PATH).
    #[clap(short, long)]
    name: Option<String>,

    /// Generate the pipeline into an ESP32 firmware for this chip (`esp32c5`,
    /// `esp32c6`, `esp32c61`) instead of a standalone Linux pipeline project.
    #[clap(short, long, num_args = 0..=1, default_missing_value = "esp32c6")]
    pub(super) firmware: Option<Chip>,

    /// Where the myrmic crates come from: a version, a git URL or a local
    /// checkout.
    #[clap(long, alias = "repo")]
    sdk: Option<String>,
}

pub fn handle(ctx: &Ctx, cmd: New) -> anyhow::Result<()> {
    let New {
        path,
        name,
        firmware,
        sdk,
    } = cmd;

    let name = determine_name(name.as_deref(), &path)?;
    validate_name(name)?;

    match firmware {
        Some(chip) => crate::info!(ctx, "Creating firmware pipeline '{}' for {}", name, chip),
        None => crate::info!(ctx, "Creating Linux pipeline '{}'", name),
    }

    let repo = crate::utils::resolve_repo(ctx, sdk.as_deref())?;
    let dep = |known_path: &str| repo.clone().resolve_or_assume_correct(known_path);

    match firmware {
        Some(chip) => {
            let BoardPins {
                scl,
                sda,
                general_purpose,
            } = board_pins(chip.name())?;
            let modules: Vec<_> = shipped_modules(&repo).collect();
            let pipeline_feature_deps =
                models::DepFeatures(modules.iter().map(|(name, _)| name.clone()).collect());
            let module_deps = models::CargoDeps(
                modules
                    .into_iter()
                    .map(|(name, dep)| (name, dep.optional()))
                    .collect(),
            );

            render_project(
                &TemplateNewFirmwarePipeline {
                    name,
                    chip: chip.name(),
                    scl,
                    sda,
                    general_purpose,
                    firmware_sdk: dep("embedded/esp-hal/crates/esp-firmware"),
                    firmware_build: dep("embedded/esp-hal/crates/esp-firmware-build"),
                    module_deps,
                    pipeline_feature_deps,
                    toolchain: myrmic_build::TOOLCHAIN,
                },
                &path,
            )?;
            write_partitions(chip, &path)?;
        }
        None => render_project(
            &TemplateNewLinuxPipeline {
                name,
                signal_layer_ipc: dep("sdk/signal-layer/signal-layer-ipc"),
                signal_layer_linux_rt: dep("swarm/signal-layer/signal-layer-linux-rt"),
                linux_i2c_shim: dep("swarm/signal-layer/linux-i2c-shim"),
                linux_gpio_shim: dep("swarm/signal-layer/linux-gpio-shim"),
                linux_spi_shim: dep("swarm/signal-layer/linux-spi-shim"),
                signal_layer_core: dep("sdk/signal-layer/signal-layer-core"),
                signal_layer_types: dep("sdk/signal-layer/signal-layer-types")
                    .renamed_from("myrmic-signal-layer-types"),
                module_deps: models::CargoDeps(shipped_modules(&repo).collect()),
                linux_codegen: dep("sdk/signal-layer/linux-codegen"),
            },
            &path,
        )?,
    }

    seed_lockfile(&repo, &path)
}

/// Every driver and step shipped with myrmic as `(crate name, dependency)`,
/// seeded into the pipeline scaffolds so any pipeline built from their YAMLs
/// compiles.
fn shipped_modules(repo: &models::Repo) -> impl Iterator<Item = (String, models::CargoDep)> + '_ {
    let drivers = esp_codegen::driver_ids().into_iter().map(|id| {
        (
            format!("{id}-driver"),
            format!("signal-modules/drivers/{id}"),
        )
    });
    let steps = esp_codegen::step_ids().into_iter().map(|id| {
        let path = format!("signal-modules/steps/{id}");
        (id, path)
    });
    drivers
        .chain(steps)
        .map(move |(name, path)| (name, repo.clone().resolve_or_assume_correct(&path)))
}
