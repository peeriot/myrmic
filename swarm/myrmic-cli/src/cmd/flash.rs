use std::io::IsTerminal as _;
use std::path::PathBuf;

use anyhow::Context as _;
use espflash::cli::ConnectArgs;
use myrmic_build::firmware;

use crate::args::Ctx;
use crate::flash::{self, Board};
use crate::utils::{PathType, determine_wd};
use crate::{build, models};

#[derive(clap::Parser)]
pub struct Flash {
    /// Path to the firmware crate directory or Cargo.toml to build and flash (defaults to the current directory).
    path: Option<PathBuf>,

    #[clap(flatten)]
    connect: ConnectArgs,

    /// Which cargo target to build: a binary name. Omit for a crate with a single binary.
    #[clap(long)]
    target: Option<models::CargoTarget>,

    /// After flashing, stream the board's serial output until interrupted.
    #[clap(short, long)]
    monitor: bool,
}

pub fn handle(ctx: Ctx, cmd: Flash) -> anyhow::Result<()> {
    let Flash {
        path,
        mut connect,
        target,
        monitor,
    } = cmd;

    let path = determine_wd(ctx, path)?;
    let (manifest, ty) = PathType::from_path(&path)?;
    if !matches!(ty, PathType::Toml) {
        anyhow::bail!("not a firmware crate: {}", path.display());
    }
    let chip = firmware::chip_of(&manifest)?.with_context(|| {
        format!(
            "`{}` is not a firmware crate: `[package.metadata.myrmic] firmware` names no chip \
             (a cell is deployed with `myrmic deploy`, not flashed)",
            manifest.display()
        )
    })?;

    crate::log::adopt_log_crate(ctx);
    // With nobody to ask, espflash refuses a choice of ports and points at --port.
    if !std::io::stdin().is_terminal() {
        connect.non_interactive = true;
    }
    let config = flash::config()?;
    let mut board = Board::connect(&connect, &config)?;
    if board.chip() != flash::espflash_chip(chip) {
        anyhow::bail!(
            "the board is an {}, but `{}` is built for the {chip}",
            board.chip(),
            manifest.display()
        );
    }
    let info = board.info()?;

    let cargo_target = build::to_build_cargo_target(target.unwrap_or(models::CargoTarget::Auto));
    crate::info!(ctx, "Building {chip} firmware: {}", manifest.display());
    let flash_size = u64::from(info.flash_size.size());
    let built = firmware::build(&manifest, &cargo_target, Some(flash_size))?;
    build::report_layout(ctx, &built);

    crate::info!(ctx, "Flashing {}...", built.elf.display());
    let elf = std::fs::read(&built.elf)
        .with_context(|| format!("failed to read {}", built.elf.display()))?;
    let port_name = board.flash(&elf, built.partition_table.as_deref(), &info)?;
    crate::info!(ctx, "Flashed; the board is booting");

    if monitor {
        let rom = info.rom();
        let elfs = std::iter::once(elf.as_slice())
            .chain(rom.as_deref())
            .collect();
        flash::monitor(ctx, &port_name, elfs)?;
    }
    Ok(())
}
