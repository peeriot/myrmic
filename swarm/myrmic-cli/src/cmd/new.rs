use myrmic_build::firmware::Chip;

use crate::args::Ctx;
use crate::determine_name;
use crate::utils::validate_name;

mod project;

pub(crate) use project::{render_project, seed_lockfile, write_partitions};

#[derive(clap::Parser)]
pub struct New {
    path: std::path::PathBuf,

    #[clap(short, long)]
    name: Option<String>,

    /// Scaffold a firmware crate instead of a cell, with this chip (`esp32c5`,
    /// `esp32c6`, `esp32c61`) as its default feature.
    #[clap(short, long, num_args = 0..=1, default_missing_value = "esp32c6")]
    firmware: Option<Chip>,

    #[clap(long, alias = "repo")]
    sdk: Option<String>,
}

pub fn handle(ctx: &Ctx, cmd: New) -> anyhow::Result<()> {
    let New {
        path,
        name,
        sdk: repo,
        firmware,
    } = cmd;

    let name = determine_name(name.as_deref(), &path)?;

    validate_name(name)?;

    match firmware {
        Some(chip) => crate::info!(ctx, "Creating firmware '{}' for {}", name, chip),
        None => crate::info!(ctx, "Creating '{}'", name),
    }

    let repo = crate::utils::resolve_repo(ctx, repo.as_deref())?;
    project::render(&repo, name, firmware, &path)?;

    if let Some(chip) = firmware {
        write_partitions(chip, &path)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    /// The Signal Layer scaffolds live under `myrmic signal-layer`; `new` no
    /// longer knows the flags.
    #[test]
    fn the_signal_layer_flags_are_gone() {
        for args in [
            ["new", "foo", "--pipeline"],
            ["new", "foo", "--driver=i2c"],
            ["new", "foo", "--step"],
            ["new", "foo", "--registry-dir=."],
        ] {
            assert!(New::try_parse_from(args).is_err(), "{args:?}");
        }
    }
}
