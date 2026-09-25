use myrmic_build::firmware;

use crate::args::Ctx;
use crate::platforms::Platform;
use crate::utils::determine_wd;
use crate::{PathType, build, models, nest};
use std::path::PathBuf;

#[derive(clap::Parser)]
pub struct Build {
    /// Path to the cell or firmware directory, Cargo.toml, or `app_specs.yml` to build (defaults to the current directory).
    path: Option<PathBuf>,
    /// Comma-separated list of build platforms to compile for (e.g. `linux`).
    /// Ignored for a firmware crate, whose chip comes from its manifest.
    #[clap(long)]
    platform: Option<String>,

    /// Which cargo target to build: `lib` or a target name. Omit to auto-select (sole bin, else sole lib).
    #[clap(long)]
    target: Option<models::CargoTarget>,

    /// For an app spec, the app name (the bundle's grouping name; also names the
    /// generated `.nest`); otherwise the manifest `name:`, else the app folder.
    /// For a firmware crate, the name the device registers with, baked into the
    /// image; otherwise the chip's name.
    #[clap(short = 'n', long)]
    name: Option<String>,

    /// Currently only firmware crates use them.
    #[clap(flatten)]
    features: FeatureArgs,
}

/// The cargo feature flags of the commands that build a firmware crate.
#[derive(clap::Args)]
pub struct FeatureArgs {
    /// Extra cargo features to enable, comma-separated or repeated. Added on top
    /// of the crate's default features unless `--no-default-features` is given.
    #[clap(long, value_delimiter = ',')]
    pub(crate) features: Vec<String>,

    /// Leaves the crate's default features off.
    #[clap(long)]
    pub(crate) no_default_features: bool,
}

pub fn handle(ctx: &Ctx, cmd: Build) -> anyhow::Result<()> {
    let Build {
        path,
        platform,
        target,
        name,
        features,
    } = cmd;
    let features = firmware::Features::from(features);

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
                if !features.is_default() {
                    crate::warn!(
                        ctx,
                        "--features or --no-default-features was provided, but will be \
                         ignored for an app"
                    );
                }

                let info = build::build_app(ctx, &path, app, name.as_deref())?;

                let out = format!("./{}.nest", info.name);
                nest::write(ctx, out, info)?;
            }
        },
        (path, PathType::Toml) => {
            let cargo_target = target.unwrap_or(models::CargoTarget::Auto);
            let platforms = Platform::parse_list(platform.as_deref())?;

            let _classes = build::build_toml(
                ctx,
                &path,
                &platforms,
                cargo_target,
                name.as_deref(),
                &features,
            )?;
        }
        (_path, PathType::Nest | PathType::Wasm) => {
            anyhow::bail!("not a valid build target: {}", path.display());
        }
    }

    Ok(())
}

impl From<FeatureArgs> for firmware::Features {
    fn from(args: FeatureArgs) -> Self {
        Self {
            features: args.features,
            no_default_features: args.no_default_features,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[test]
    fn n_is_short_for_the_name() {
        let build = Build::try_parse_from(["build", "-n", "kitchen"]).unwrap();
        assert_eq!(build.name.as_deref(), Some("kitchen"));
    }

    #[test]
    fn features_split_on_commas_and_repeat() {
        let build =
            Build::try_parse_from(["build", "--features", "pipeline,wdt-selftest"]).unwrap();
        assert_eq!(build.features.features, ["pipeline", "wdt-selftest"]);

        let build = Build::try_parse_from([
            "build",
            "--features",
            "pipeline",
            "--features",
            "wdt-selftest",
        ])
        .unwrap();
        assert_eq!(build.features.features, ["pipeline", "wdt-selftest"]);
    }

    #[test]
    fn default_features_stay_on_unless_disabled() {
        let build = Build::try_parse_from(["build"]).unwrap();
        assert!(!build.features.no_default_features);

        let build = Build::try_parse_from(["build", "--no-default-features"]).unwrap();
        assert!(build.features.no_default_features);
    }
}
