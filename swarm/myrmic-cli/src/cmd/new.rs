use anyhow::Context as _;
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

    /// Also scaffold a Signal Layer pipeline (board.yml + pipeline.yml, generated
    /// into the firmware at build time). Requires `--firmware`.
    #[clap(long)]
    pipeline: bool,

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

#[derive(textus::Template)]
#[template(path = "templates/firmware-pipeline", strip_suffix = ".tmpl")]
struct TemplateNewFirmwarePipeline<'a> {
    name: &'a str,
    chip: &'a str,
    firmware_sdk: models::CargoDep,
    firmware_build: models::CargoDep,
    module_deps: String,
    pipeline_feature_deps: String,
}

#[derive(textus::Template)]
#[template(path = "templates/linux-pipeline", strip_suffix = ".tmpl")]
struct TemplateNewLinuxPipeline<'a> {
    name: &'a str,
    runtime_deps: String,
    module_deps: String,
    linux_codegen: models::CargoDep,
}

pub fn handle(ctx: Ctx, cmd: New) -> anyhow::Result<()> {
    let New {
        path,
        name,
        sdk: repo,
        firmware,
        pipeline,
    } = cmd;

    let name = determine_name(name.as_deref(), &path)?;

    validate_name(name)?;

    match (firmware, pipeline) {
        (Some(chip), true) => {
            crate::info!(ctx, "Creating firmware pipeline '{}' for {}", name, chip);
        }
        (Some(chip), false) => crate::info!(ctx, "Creating firmware '{}' for {}", name, chip),
        (None, true) => crate::info!(ctx, "Creating Linux pipeline '{}'", name),
        (None, false) => crate::info!(ctx, "Creating '{}'", name),
    }

    let repo = crate::utils::resolve_repo(ctx, repo.as_deref())?;

    render_project(repo, name, firmware, pipeline, &path)?;

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

    match (firmware, pipeline) {
        (Some(chip), true) => write_pipeline_yamls(name, &path, chip)?,
        (None, true) => write_linux_pipeline_yamls(name, &path)?,
        _ => {}
    }

    Ok(())
}

/// Renders the selected project template into `path`, cleaning up the directory
/// on failure.
fn render_project(
    repo: models::Repo,
    name: &str,
    firmware: Option<Chip>,
    pipeline: bool,
    path: &std::path::Path,
) -> anyhow::Result<()> {
    // Only a directory we create is ours to clean up on failure; never delete a
    // directory the user pointed us at that already held files.
    let preexisting = path.exists();

    let result = match (firmware, pipeline) {
        (Some(chip), true) => {
            let firmware_sdk = repo
                .clone()
                .resolve_or_assume_correct("embedded/esp-hal/crates/esp-firmware");
            let firmware_build = repo
                .clone()
                .resolve_or_assume_correct("embedded/esp-hal/crates/esp-firmware-build");
            let (module_deps, pipeline_feature_deps) = module_dep_lines(&repo);

            TemplateNewFirmwarePipeline {
                name,
                chip: chip.name(),
                firmware_sdk,
                firmware_build,
                module_deps,
                pipeline_feature_deps,
            }
            .render_into(path)
        }
        (Some(chip), false) => {
            let firmware_sdk = repo
                .clone()
                .resolve_or_assume_correct("embedded/esp-hal/crates/esp-firmware");
            let firmware_build =
                repo.resolve_or_assume_correct("embedded/esp-hal/crates/esp-firmware-build");

            TemplateNewFirmware {
                name,
                chip: chip.name(),
                firmware_sdk,
                firmware_build,
            }
            .render_into(path)
        }
        (None, true) => {
            let linux_codegen = repo
                .clone()
                .resolve_or_assume_correct("sdk/signal-layer/linux-codegen");
            let runtime_deps = linux_runtime_deps(&repo);
            let module_deps = linux_module_deps(&repo);

            TemplateNewLinuxPipeline {
                name,
                runtime_deps,
                module_deps,
                linux_codegen,
            }
            .render_into(path)
        }
        (None, false) => {
            let sdk = repo.resolve_or_assume_correct("sdk/myrmic-sdk");

            TemplateNew {
                name,
                myrmic_sdk: sdk,
            }
            .render_into(path)
        }
    };

    if let Err(err) = result {
        if !preexisting && let Err(io_err) = std::fs::remove_dir_all(path) {
            return Err(anyhow::Error::new(io_err).context(format!(
                "unable to cleanup after template render failure: {err}"
            )));
        }
        return Err(anyhow::Error::new(err).context("failed to render template"));
    }

    Ok(())
}

/// Renders the driver/step dependency lines and the `pipeline` feature's
/// `dep:` list, seeding every module shipped with myrmic so any pipeline built
/// from them compiles.
fn module_dep_lines(repo: &models::Repo) -> (String, String) {
    let modules = esp_codegen::driver_ids()
        .into_iter()
        .map(|id| {
            (
                format!("{id}-driver"),
                format!("signal-modules/drivers/{id}"),
            )
        })
        .chain(
            esp_codegen::step_ids()
                .into_iter()
                .map(|id| (id.clone(), format!("signal-modules/steps/{id}"))),
        );

    let mut deps = Vec::new();
    let mut feats = Vec::new();
    for (crate_name, rel_path) in modules {
        let dep = repo.clone().resolve_or_assume_correct(&rel_path);
        deps.push(format!("{crate_name} = {}", optional_dep(&dep)));
        feats.push(format!("    \"dep:{crate_name}\","));
    }
    (deps.join("\n"), feats.join("\n"))
}

/// Renders a [`models::CargoDep`] as a dependency table with `optional = true`.
fn optional_dep(dep: &models::CargoDep) -> String {
    let rendered = dep.to_string();
    match rendered.strip_suffix(" }") {
        Some(inner) => format!("{inner}, optional = true }}"),
        None => format!("{{ version = {rendered}, optional = true }}"),
    }
}

/// Writes `board.yml` and `pipeline.yml` next to a scaffolded pipeline firmware.
fn write_pipeline_yamls(name: &str, path: &std::path::Path, chip: Chip) -> anyhow::Result<()> {
    let board = generate_board_yaml(name, chip.name())?;
    std::fs::write(path.join("board.yml"), board)
        .with_context(|| format!("writing {}", path.join("board.yml").display()))?;
    std::fs::write(path.join("pipeline.yml"), generate_pipeline_yaml(name))
        .with_context(|| format!("writing {}", path.join("pipeline.yml").display()))?;
    Ok(())
}

/// A starter board manifest: an example i2c bus, every usable GPIO for the chip
/// (minus the bus pins) offered to the cell, and a headless `sim-source` device.
fn generate_board_yaml(name: &str, chip: &str) -> anyhow::Result<String> {
    let usable = esp_codegen::chip_general_purpose_pins(chip)
        .with_context(|| format!("no GPIO layout for chip `{chip}`"))?;
    let (scl, sda) = pick_bus_pins(&usable)?;
    let gp = usable
        .iter()
        .copied()
        .filter(|pin| *pin != scl && *pin != sda)
        .map(|pin| pin.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "# Board manifest: the hardware the Signal Layer pipeline runs on.\n\
         id: {name}\n\
         chip: {chip}\n\
         \n\
         # Buses the pipeline may use. The sim-source device below is bound to\n\
         # this bus for codegen symmetry but never touches it, so nothing needs\n\
         # to be wired up to build and run.\n\
         buses:\n\
         \x20\x20i2c0:\n\
         \x20\x20\x20\x20transport: i2c\n\
         \x20\x20\x20\x20pins:\n\
         \x20\x20\x20\x20\x20\x20scl: {scl}\n\
         \x20\x20\x20\x20\x20\x20sda: {sda}\n\
         \x20\x20\x20\x20freq_khz: 400\n\
         \n\
         # Every usable GPIO on this chip, minus the bus pins above. Pins listed\n\
         # here that the pipeline does not claim are offered to the cell.\n\
         gpios:\n\
         \x20\x20general_purpose: [{gp}]\n\
         \n\
         # Devices on the buses. Swap sim-source for a real sensor once wired up.\n\
         devices:\n\
         \x20\x20- id: sim\n\
         \x20\x20\x20\x20driver: sim-source\n\
         \x20\x20\x20\x20bus: i2c0\n"
    ))
}

/// Picks the example i2c bus pins: the conventional GPIO10/GPIO11 when both are
/// usable, else the first two usable pins on the chip.
fn pick_bus_pins(usable: &[u8]) -> anyhow::Result<(u8, u8)> {
    if usable.contains(&10) && usable.contains(&11) {
        return Ok((10, 11));
    }
    let mut it = usable.iter().copied();
    let scl = it.next().context("chip has no usable GPIOs")?;
    let sda = it
        .next()
        .context("chip needs at least two usable GPIOs for the example bus")?;
    Ok((scl, sda))
}

/// A starter pipeline: the headless `sim-source` ramp exposed as one tap.
fn generate_pipeline_yaml(name: &str) -> String {
    format!(
        "# Signal Layer pipeline: sources, steps, and the taps the cell reads.\n\
         pipeline:\n\
         \x20\x20id: {name}\n\
         \n\
         # `sim` emits a deterministic ramp (0, 10, ... 100, 0, ...) so this\n\
         # builds and runs with no hardware attached.\n\
         sources:\n\
         \x20\x20- id: sim\n\
         \x20\x20\x20\x20device: sim\n\
         \x20\x20\x20\x20config:\n\
         \x20\x20\x20\x20\x20\x20sample_interval_ms: 500\n\
         \x20\x20\x20\x20\x20\x20start: 0.0\n\
         \x20\x20\x20\x20\x20\x20step: 10.0\n\
         \x20\x20\x20\x20\x20\x20max: 100.0\n\
         \n\
         # A tap is a value the cell can read by name.\n\
         taps:\n\
         \x20\x20- name: sim_value\n\
         \x20\x20\x20\x20kind: retained\n\
         \x20\x20\x20\x20type: f32\n\
         \x20\x20\x20\x20source: sim.value\n"
    )
}

/// Renders the Linux Signal Layer runtime dependency lines: git or path for the
/// peeriot crates, versions for the crates.io ones.
fn linux_runtime_deps(repo: &models::Repo) -> String {
    let dep = |path: &str| repo.clone().resolve_or_assume_correct(path);
    let mut lines = vec![
        format!(
            "signal-layer-ipc      = {}",
            dep("sdk/signal-layer/signal-layer-ipc")
        ),
        format!(
            "signal-layer-linux-rt = {}",
            dep("swarm/signal-layer/signal-layer-linux-rt")
        ),
        format!(
            "linux-i2c-shim        = {}",
            dep("swarm/signal-layer/linux-i2c-shim")
        ),
        format!(
            "linux-gpio-shim       = {}",
            dep("swarm/signal-layer/linux-gpio-shim")
        ),
        format!(
            "linux-spi-shim        = {}",
            dep("swarm/signal-layer/linux-spi-shim")
        ),
        format!(
            "signal-layer-core     = {}",
            dep("sdk/signal-layer/signal-layer-core")
        ),
        format!(
            "signal-layer-types    = {}",
            with_package(
                &dep("sdk/signal-layer/signal-layer-types"),
                "myrmic-signal-layer-types"
            )
        ),
    ];
    lines.push(String::from(
        "tokio                 = { version = \"1\", features = [\"full\"] }",
    ));
    lines.push(String::from(
        "tokio-stream          = { version = \"0.1\", features = [\"time\"] }",
    ));
    lines.push(String::from(
        "critical-section      = { version = \"1\", features = [\"std\"] }",
    ));
    lines.push(String::from(
        "log                   = { version = \"0.4\", default-features = false }",
    ));
    lines.push(String::from("env_logger            = \"0.11\""));
    // Real drivers use embassy-time async delays; on the tokio runtime the std
    // driver plus a timer queue provide the backend embedded gets from esp-rtos.
    lines.push(String::from(
        "embassy-time          = { version = \"0.5\", features = [\"std\", \"generic-queue-8\"] }",
    ));
    lines.join("\n")
}

/// Renders a [`models::CargoDep`] table with a `package = "..."` rename added.
fn with_package(dep: &models::CargoDep, package: &str) -> String {
    let rendered = dep.to_string();
    match rendered.strip_suffix(" }") {
        Some(inner) => format!("{inner}, package = \"{package}\" }}"),
        None => format!("{{ version = {rendered}, package = \"{package}\" }}"),
    }
}

/// Renders the driver/step dependency lines for a Linux pipeline: every shipped
/// module, so any pipeline built from the YAMLs compiles.
fn linux_module_deps(repo: &models::Repo) -> String {
    esp_codegen::driver_ids()
        .into_iter()
        .map(|id| {
            (
                format!("{id}-driver"),
                format!("signal-modules/drivers/{id}"),
            )
        })
        .chain(
            esp_codegen::step_ids()
                .into_iter()
                .map(|id| (id.clone(), format!("signal-modules/steps/{id}"))),
        )
        .map(|(name, path)| format!("{name} = {}", repo.clone().resolve_or_assume_correct(&path)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Writes `manifest.yml` and `pipeline.yml` for a Linux pipeline project.
fn write_linux_pipeline_yamls(name: &str, path: &std::path::Path) -> anyhow::Result<()> {
    std::fs::write(
        path.join("manifest.yml"),
        generate_linux_manifest_yaml(name),
    )
    .with_context(|| format!("writing {}", path.join("manifest.yml").display()))?;
    std::fs::write(path.join("pipeline.yml"), generate_pipeline_yaml(name))
        .with_context(|| format!("writing {}", path.join("pipeline.yml").display()))?;
    Ok(())
}

/// A starter Linux device manifest: an i2c bus by `dev_path` and a headless
/// sim-source device.
fn generate_linux_manifest_yaml(name: &str) -> String {
    format!(
        "# Linux device manifest: the devices the pipeline reads.\n\
         id: {name}\n\
         chip: linux\n\
         \n\
         # I2C buses by their Linux character device.\n\
         buses:\n\
         \x20\x20i2c0:\n\
         \x20\x20\x20\x20transport: i2c\n\
         \x20\x20\x20\x20pins: {{}}\n\
         \x20\x20\x20\x20freq_khz: 400\n\
         \x20\x20\x20\x20dev_path: /dev/i2c-1\n\
         \n\
         gpios:\n\
         \x20\x20general_purpose: []\n\
         \n\
         # sim-source is synthetic and ignores the bus, so this builds and runs\n\
         # with no hardware. Swap it for a real sensor and set the bus dev_path.\n\
         devices:\n\
         \x20\x20- id: sim\n\
         \x20\x20\x20\x20driver: sim-source\n\
         \x20\x20\x20\x20bus: i2c0\n"
    )
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
