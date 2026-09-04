use crate::args::Ctx;
use crate::platforms::Platform;
use crate::utils::determine_wd;
use crate::{PathType, build, models, nest};
use std::path::PathBuf;

#[derive(clap::Parser)]
pub struct Build {
    /// Path to the cell directory, Cargo.toml, or `app_specs.yml` to build (defaults to the current directory).
    path: Option<PathBuf>,
    /// Comma-separated list of build platforms to compile for (e.g. `linux`).
    #[clap(long)]
    platform: Option<String>,

    /// Which cargo target to build: `lib` or a target name. Omit to auto-select (sole bin, else sole lib).
    #[clap(long)]
    target: Option<models::CargoTarget>,

    /// Override the app name (the bundle's grouping name; also names the
    /// generated `.nest`). Otherwise the manifest `name:`, else the app folder.
    #[clap(long)]
    name: Option<String>,
}

pub fn handle(ctx: Ctx, cmd: Build) -> anyhow::Result<()> {
    let Build {
        path,
        platform,
        target,
        name,
    } = cmd;

    let path = determine_wd(ctx, path)?;

    match PathType::from_path(&path)? {
        (path, PathType::Yaml) => match crate::parse_from_file(&path)? {
            models::BuildInput::App(app) => {
                if platform.is_some() {
                    crate::warn!(ctx, "--platform was provided, but will be ignored");
                }
                if target.is_some() {
                    crate::warn!(
                        ctx,
                        "--target was provided, but will be ignored (set `target:` per cell_class)"
                    );
                }

                let info = build::build_app(ctx, &path, app, name.as_deref())?;

                let out = format!("./{}.nest", info.name);
                nest::write(ctx, out, info)?;
            }
        },
        (path, PathType::Toml) => {
            if name.is_some() {
                crate::warn!(ctx, "--name was provided, but will be ignored");
            }

            let cargo_target = target.unwrap_or(models::CargoTarget::Auto);
            let platforms = Platform::parse_list(platform.as_deref())?;

            let _classes = build::build_toml(ctx, &path, &platforms, cargo_target)?;
        }
        (_path, PathType::Nest | PathType::Wasm) => {
            anyhow::bail!("not a valid build target: {}", path.display());
        }
    }

    Ok(())
}
