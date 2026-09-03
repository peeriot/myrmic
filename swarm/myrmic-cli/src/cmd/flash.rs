use std::io::IsTerminal as _;
use std::path::PathBuf;

use anyhow::Context as _;
use myrmic_build::firmware::{self, spell_size};
use serialport::SerialPortInfo;

use crate::args::Ctx;
use crate::flash::{self, PortChoice};
use crate::utils::{PathType, determine_wd};
use crate::{build, models};

#[derive(clap::Parser)]
pub struct Flash {
    /// Path to the firmware crate directory or Cargo.toml to build and flash (defaults to the current directory).
    path: Option<PathBuf>,

    /// Serial port of the board. Defaults to `ESPFLASH_PORT`, else the only USB serial port present.
    #[clap(short, long)]
    port: Option<String>,

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
        port,
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

    let requested = port.or_else(|| {
        std::env::var("ESPFLASH_PORT")
            .ok()
            .filter(|port| !port.is_empty())
    });
    let port = match flash::choose_port(requested.as_deref(), serialport::available_ports()?)? {
        PortChoice::Port(port) => port,
        PortChoice::Ambiguous(ports) => pick_port(ports)?,
    };

    crate::info!(ctx, "Connecting to {}...", flash::describe(&port));
    let board = flash::Board::connect(&port)?;
    if board.chip() != flash::espflash_chip(chip) {
        anyhow::bail!(
            "the board on {} is an {}, but `{}` is built for the {chip}",
            board.port_name(),
            board.chip(),
            manifest.display()
        );
    }
    let flash_size = board.flash_size().map(|size| u64::from(size.size()));
    match flash_size {
        Some(size) => crate::info!(ctx, "Found {chip} with {} of flash", spell_size(size)),
        None => crate::warn!(
            ctx,
            "Found {chip}, but its flash size could not be detected; assuming 4M"
        ),
    }

    let cargo_target = build::to_build_cargo_target(target.unwrap_or(models::CargoTarget::Auto));
    crate::info!(ctx, "Building {chip} firmware: {}", manifest.display());
    let built = firmware::build(&manifest, &cargo_target, flash_size)?;
    build::report_layout(ctx, &built);

    crate::info!(ctx, "Flashing {}...", built.elf.display());
    let port_name = board.port_name().to_owned();
    board.flash(ctx, &built.elf, built.partition_table.as_deref())?;
    crate::info!(ctx, "Flashed; the board is booting");

    if monitor {
        flash::monitor(ctx, &port_name)?;
    }
    Ok(())
}

/// Several USB serial ports: ask when there is someone to ask, else refuse and
/// list the candidates.
fn pick_port(mut ports: Vec<SerialPortInfo>) -> anyhow::Result<SerialPortInfo> {
    let labels: Vec<String> = ports.iter().map(flash::describe).collect();
    if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        let index = dialoguer::Select::new()
            .with_prompt("Several USB serial ports found; which one is the board?")
            .items(&labels)
            .default(0)
            .interact()?;
        Ok(ports.swap_remove(index))
    } else {
        anyhow::bail!(
            "several USB serial ports found; name the board's with --port:\n  {}",
            labels.join("\n  ")
        )
    }
}
