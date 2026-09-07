use myrmic_build::firmware::Chip;
use textus::Template as _;

use crate::args::Ctx;
use crate::{determine_name, models};

#[derive(clap::Parser)]
pub struct New {
    path: std::path::PathBuf,

    #[clap(short, long)]
    name: Option<String>,

    /// Scaffold a firmware crate for this chip (`esp32c5`, `esp32c6`, `esp32c61`)
    /// instead of a cell.
    #[clap(short, long, require_equals = true, num_args = 0..=1, default_missing_value = "esp32c6")]
    firmware: Option<Chip>,

    #[clap(long, alias = "repo")]
    sdk: Option<String>,
}

#[derive(textus::Template)]
#[template(path = "templates/new", strip_suffix = ".tmpl")]
struct TemplateNew<'a> {
    name: &'a str,
    myrmic_sdk: models::CargoDep,
}

#[derive(textus::Template)]
#[template(path = "templates/firmware", strip_suffix = ".tmpl")]
struct TemplateNewFirmware<'a> {
    name: &'a str,
    chip: &'a str,
    firmware_sdk: models::CargoDep,
    firmware_build: models::CargoDep,
}

pub fn handle(ctx: Ctx, cmd: New) -> anyhow::Result<()> {
    let New {
        path,
        name,
        sdk: repo,
        firmware,
    } = cmd;

    let name = determine_name(name.as_deref(), &path)?;

    validate_name(name)?;

    if let Some(chip) = firmware {
        crate::info!(ctx, "Creating firmware '{}' for {}", name, chip);
    } else {
        crate::info!(ctx, "Creating '{}'", name);
    }

    let repo = crate::utils::resolve_repo(ctx, repo.as_deref())?;

    let result = if let Some(chip) = firmware {
        let firmware_sdk = repo
            .clone()
            .resolve_or_assume_correct("embedded/esp-hal/crates/esp-firmware");
        let firmware_build =
            repo.resolve_or_assume_correct("embedded/esp-hal/crates/esp-firmware-build");

        let template = TemplateNewFirmware {
            name,
            chip: chip.name(),
            firmware_sdk,
            firmware_build,
        };

        template.render_into(&path)
    } else {
        let sdk = repo.resolve_or_assume_correct("sdk/myrmic-sdk");

        let template = TemplateNew {
            name,
            myrmic_sdk: sdk,
        };

        template.render_into(&path)
    };

    if let Err(err) = result {
        if let Err(io_err) = std::fs::remove_dir_all(&path) {
            return Err(anyhow::Error::new(io_err).context(format!(
                "unable to cleanup after template render failure: {}",
                err
            )));
        }
        return Err(anyhow::Error::new(err).context("failed to render template"));
    }

    // A firmware crate also gets the partition layout for its chip - the same
    // per-chip files `modem-esp32` builds with (C5/C6 = 4 MB, C61 = 8 MB).
    if let Some(chip) = firmware {
        let partitions = match chip {
            Chip::Esp32c5 => {
                include_str!("../../../../embedded/esp-hal/modem-esp32/partitions/esp32c5.toml")
            }
            Chip::Esp32c6 => {
                include_str!("../../../../embedded/esp-hal/modem-esp32/partitions/esp32c6.toml")
            }
            Chip::Esp32c61 => {
                include_str!("../../../../embedded/esp-hal/modem-esp32/partitions/esp32c61.toml")
            }
        };
        let dst = path.join("partitions.toml");
        if let Err(e) = std::fs::write(&dst, partitions) {
            return Err(anyhow::Error::new(e).context(format!("failed to write {}", dst.display())));
        }
    }

    Ok(())
}

fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("<name> is empty");
    }

    let mut chars = name.chars();
    if let Some(ch) = chars.next() {
        if ch.is_ascii_digit() {
            // Technically would be caught via `is_xid_start`, but this is a pretty standard mistake to make...
            anyhow::bail!("<name> cannot start with a digit");
        }
        if !(unicode_ident::is_xid_start(ch) || ch == '_') {
            anyhow::bail!(
                "the first character in <name> must be a Unicode XID start character (most letters or `_`)"
            );
        }
    }
    for ch in chars {
        if !(unicode_ident::is_xid_continue(ch) || ch == '-') {
            anyhow::bail!(
                "<name> must be Unicode XID characters (numbers, `-`, `_`, or most letters)"
            );
        }
    }
    Ok(())
}
