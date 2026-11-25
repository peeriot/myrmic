//! Cell and firmware project scaffolds.

use anyhow::Context as _;
use myrmic_build::firmware::Chip;
use textus::Template;

use crate::models;

#[derive(textus::Template)]
#[template(path = "templates/new", strip_suffix = ".tmpl")]
struct TemplateNew<'a> {
    name: &'a str,
    myrmic_sdk: models::CargoDep,
    toolchain: &'a str,
}

#[derive(textus::Template)]
#[template(path = "templates/firmware", strip_suffix = ".tmpl")]
struct TemplateNewFirmware<'a> {
    name: &'a str,
    chip: &'a str,
    firmware_sdk: models::CargoDep,
    firmware_build: models::CargoDep,
    toolchain: &'a str,
}

/// Renders the cell or firmware template into `path`.
pub(super) fn render(
    repo: &models::Repo,
    name: &str,
    firmware: Option<Chip>,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    let dep = |known_path: &str| repo.clone().resolve_or_assume_correct(known_path);

    match firmware {
        Some(chip) => render_project(
            &TemplateNewFirmware {
                name,
                chip: chip.name(),
                firmware_sdk: dep("embedded/esp-hal/crates/esp-firmware"),
                firmware_build: dep("embedded/esp-hal/crates/esp-firmware-build"),
                toolchain: myrmic_build::TOOLCHAIN,
            },
            path,
        )?,
        None => render_project(
            &TemplateNew {
                name,
                myrmic_sdk: dep("sdk/myrmic-sdk"),
                toolchain: myrmic_build::TOOLCHAIN,
            },
            path,
        )?,
    }

    seed_lockfile(repo, path)
}

/// Renders `template` into `path`, cleaning the directory up on failure. Only
/// a directory we create is ours to clean up; a directory the user pointed us
/// at that already held files is left alone.
pub(crate) fn render_project(
    template: &impl Template,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    let preexisting = path.exists();

    if let Err(err) = template.render_into(path) {
        if !preexisting && let Err(io_err) = std::fs::remove_dir_all(path) {
            return Err(anyhow::Error::new(io_err).context(format!(
                "unable to cleanup after template render failure: {err}"
            )));
        }
        return Err(anyhow::Error::new(err).context("failed to render template"));
    }

    Ok(())
}

/// Copies a local SDK's `Cargo.lock` into a fresh scaffold, so the first build
/// starts from the versions the SDK is tested with rather than pulling in a
/// crate released since. A lockfile the user already has is left alone; a
/// registry or git SDK seeds nothing.
pub(crate) fn seed_lockfile(repo: &models::Repo, path: &std::path::Path) -> anyhow::Result<()> {
    let sdk_lock = match repo {
        models::Repo::Path(sdk) => sdk.join("Cargo.lock"),
        models::Repo::Version(_) | models::Repo::Git(..) => return Ok(()),
    };
    let dst = path.join("Cargo.lock");
    if sdk_lock.exists() && !dst.exists() {
        std::fs::copy(&sdk_lock, &dst)
            .with_context(|| format!("failed to copy {}", sdk_lock.display()))?;
    }
    Ok(())
}

/// Writes the partition layout for `chip` next to a firmware scaffold: the same
/// per-chip files `modem-esp32` builds with (C5/C6 = 4 MB, C61 = 8 MB).
pub(crate) fn write_partitions(chip: Chip, path: &std::path::Path) -> anyhow::Result<()> {
    let partitions = match chip {
        Chip::Esp32c5 => {
            include_str!("../../../../../embedded/esp-hal/modem-esp32/partitions/esp32c5.toml")
        }
        Chip::Esp32c6 => {
            include_str!("../../../../../embedded/esp-hal/modem-esp32/partitions/esp32c6.toml")
        }
        Chip::Esp32c61 => {
            include_str!("../../../../../embedded/esp-hal/modem-esp32/partitions/esp32c61.toml")
        }
    };
    let dst = path.join("partitions.toml");
    std::fs::write(&dst, partitions).with_context(|| format!("failed to write {}", dst.display()))
}
