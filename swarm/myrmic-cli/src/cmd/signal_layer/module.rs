//! Out-of-tree Signal Layer driver and step scaffolds.

use anyhow::Context as _;
use textus::Template as _;

use crate::args::Ctx;
use crate::models;
use crate::utils::validate_name;

/// The bus a scaffolded driver talks over.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub(super) enum Bus {
    I2c,
    Spi,
}

/// What every module scaffold takes.
#[derive(clap::Args)]
pub(super) struct ModuleArgs {
    /// The module id, e.g. `bme280`. The crate is created at
    /// `<registry-dir>/<drivers|steps>/<ID>`.
    pub(super) id: std::path::PathBuf,

    /// Base directory the module is placed under, as `drivers/<id>` or
    /// `steps/<id>` (default: the current directory). Point a pipeline's
    /// build.rs at this directory to enumerate the modules in it.
    #[clap(long)]
    pub(super) registry_dir: Option<std::path::PathBuf>,
}

#[derive(textus::Template)]
#[template(path = "templates/driver-i2c", strip_suffix = ".tmpl")]
struct TemplateNewDriverI2c<'a> {
    id: &'a str,
    crate_name: &'a str,
    type_name: &'a str,
}

#[derive(textus::Template)]
#[template(path = "templates/driver-spi", strip_suffix = ".tmpl")]
struct TemplateNewDriverSpi<'a> {
    id: &'a str,
    crate_name: &'a str,
    type_name: &'a str,
}

#[derive(textus::Template)]
#[template(path = "templates/step", strip_suffix = ".tmpl")]
struct TemplateNewStep<'a> {
    id: &'a str,
    crate_name: &'a str,
    type_name: &'a str,
    signal_layer_core: models::CargoDep,
}

/// A Signal Layer module scaffold: a driver on a bus, or a processing step.
#[derive(Clone, Copy)]
pub(super) enum ModuleKind {
    Driver(Bus),
    Step,
}

/// Scaffolds an out-of-tree Signal Layer driver or step under
/// `<registry_dir>/<drivers|steps>/<id>` - the layout a pipeline's `build.rs`
/// enumerates via `build_pipeline(.., Some(<registry_dir>))` / `.include(..)`.
/// `sdk` is where a step's `signal-layer-core` comes from; a driver depends on
/// no myrmic crate.
pub(super) fn scaffold(
    ctx: &Ctx,
    kind: ModuleKind,
    args: ModuleArgs,
    sdk: Option<&str>,
) -> anyhow::Result<()> {
    let ModuleArgs { id, registry_dir } = args;

    // The positional argument is the module id, not a filesystem path.
    if id.components().count() != 1 {
        anyhow::bail!(
            "pass just the module id (e.g. `bme280`), not a path; use --registry-dir for the location"
        );
    }
    let id = id
        .to_str()
        .context("cannot create a module with a non-unicode id")?;
    validate_name(id)?;

    let (subdir, kind_label, shipped, crate_name) = match kind {
        ModuleKind::Driver(_) => (
            "drivers",
            "driver",
            esp_codegen::driver_ids(),
            format!("{id}-driver"),
        ),
        ModuleKind::Step => ("steps", "step", esp_codegen::step_ids(), id.to_owned()),
    };

    if shipped.iter().any(|s| s == id) {
        crate::warn!(
            ctx,
            "a {kind_label} `{id}` already ships with myrmic; a module with the same id overlays \
             the shipped one wherever this registry is included"
        );
    }

    let base = match registry_dir {
        Some(dir) => dir,
        None => std::env::current_dir().context("cannot determine the current directory")?,
    };
    let crate_dir = base.join(subdir).join(id);
    if crate_dir.exists() {
        anyhow::bail!("{} already exists", crate_dir.display());
    }

    let repo = crate::utils::resolve_repo(ctx, sdk)?;

    crate::info!(
        ctx,
        "Creating Signal Layer {kind_label} '{id}' in {}",
        crate_dir.display()
    );
    render(kind, id, &crate_name, &repo, &crate_dir)?;

    crate::info!(
        ctx,
        "Add `{crate_name} = {{ path = \"{}\" }}` to a pipeline's dependencies and point its \
         build.rs at `{}` to build with it.",
        crate_dir.display(),
        base.display()
    );

    Ok(())
}

/// Renders the module template for `kind` into `crate_dir`, cleaning it up on
/// failure.
fn render(
    kind: ModuleKind,
    id: &str,
    crate_name: &str,
    repo: &models::Repo,
    crate_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let type_name = pascal_case(id);

    if let Some(parent) = crate_dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let result = match kind {
        ModuleKind::Driver(Bus::I2c) => TemplateNewDriverI2c {
            id,
            crate_name,
            type_name: &type_name,
        }
        .render_into(crate_dir),
        ModuleKind::Driver(Bus::Spi) => TemplateNewDriverSpi {
            id,
            crate_name,
            type_name: &type_name,
        }
        .render_into(crate_dir),
        ModuleKind::Step => {
            let signal_layer_core = repo
                .clone()
                .resolve_or_assume_correct("sdk/signal-layer/signal-layer-core");
            TemplateNewStep {
                id,
                crate_name,
                type_name: &type_name,
                signal_layer_core,
            }
            .render_into(crate_dir)
        }
    };

    if let Err(err) = result {
        if let Err(io_err) = std::fs::remove_dir_all(crate_dir) {
            return Err(anyhow::Error::new(io_err).context(format!(
                "unable to cleanup after template render failure: {err}"
            )));
        }
        return Err(anyhow::Error::new(err).context("failed to render template"));
    }

    Ok(())
}

/// `moving-average` -> `MovingAverage`, `bme280` -> `Bme280`.
fn pascal_case(id: &str) -> String {
    id.split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_from_id() {
        assert_eq!(pascal_case("bme280"), "Bme280");
        assert_eq!(pascal_case("moving-average"), "MovingAverage");
        assert_eq!(pascal_case("wsen_itds"), "WsenItds");
    }
}
