use crate::args::Ctx;
use crate::models::{self, CellInstance};
use crate::platforms::Platform;
use crate::utils::PathType;
use anyhow::Context;
use myrmic_build::{cargo, firmware};
use sorg_common::{HttpBridgeApi, MqttBridge};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Result of building a cell, so should probably be called cell artifacts...
pub struct CellClass {
    pub name: String,
    pub wasm_path: Option<PathBuf>,
    // aot, meta
    pub riscv32imac: Option<(PathBuf, PathBuf)>,
}

pub struct AppInfo {
    /// The app name every cell in this bundle is grouped under. Baked into the
    /// nest so a deployed bundle carries its own name.
    pub name: String,
    pub instances: Vec<CellInstance>,
    pub classes: HashMap<String, CellClass>,
    pub mqtt_bridges: Vec<MqttBridge>,
    pub http_bridges: Vec<HttpBridgeApi>,
}

/// The app name for a bundle: an explicit name if given (a manifest `name:` or a
/// `--name` override), else the name of the folder containing the app spec.
/// Errors when neither is available — we never invent a name.
fn resolve_app_name(explicit: Option<&str>, path: &Path) -> anyhow::Result<String> {
    if let Some(name) = explicit {
        return Ok(name.to_owned());
    }
    path.parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .context(
            "unable to determine an app name from the path; \
             provide one via the manifest `name:` field or `--name`",
        )
}

#[allow(clippy::too_many_lines)]
pub fn build_app(
    ctx: Ctx,
    path: &Path,
    app: models::App,
    name_override: Option<&str>,
) -> anyhow::Result<AppInfo> {
    let folder = path
        .parent()
        .with_context(|| format!("unable to determine folder: {}", path.display()))?;

    let models::App {
        name,
        classes,
        instances,
        ..
    } = app;

    let mut info = AppInfo {
        name: resolve_app_name(name_override.or(name.as_deref()), path)?,
        instances: Vec::new(),
        classes: Default::default(),
        mqtt_bridges: Default::default(),
        http_bridges: Default::default(),
    };

    // Resolve classes: build cell classes now; record bridge classes (id →
    // resolved spec path) so their SRI can be baked at instance-resolution time.
    let mut cell_ids: HashSet<String> = HashSet::new();
    let mut bridge_specs: HashMap<String, PathBuf> = HashMap::new();

    for class_def in classes {
        let (id, kind) = class_def.resolve()?;

        let build = match kind {
            models::ClassKind::Bridge(spec) => {
                bridge_specs.insert(id, folder.join(spec));
                continue;
            }
            models::ClassKind::Cell(build) => build,
        };

        cell_ids.insert(id.clone());
        crate::info!(ctx, "Building {}...", id);

        if !build.features.is_empty() {
            crate::warn!(ctx, "build features are currently ignored.");
        }

        let cargo_target = to_build_cargo_target(build.cargo_target()?);
        let platforms = parse_platforms(build.platforms)?;

        let path = folder.join(&build.path);
        let path = path
            .canonicalize()
            .with_context(|| format!("unable to stat {}", path.display()))?;

        let og_path = path;
        let (path, ty) = PathType::from_path(&og_path)?;

        match ty {
            PathType::Toml => {
                if let Some(chip) = firmware::chip_of(&path)? {
                    anyhow::bail!(
                        "class `{id}` points at a {chip} firmware crate, not a cell; build it on \
                         its own with `myrmic build {}`",
                        og_path.display()
                    );
                }
                if let Some(cc) = build_cell(ctx, path.as_ref(), &platforms, &cargo_target)? {
                    info.classes.insert(id, cc);
                }
            }
            PathType::Wasm => {
                anyhow::bail!("unable to convert raw wasm to aot-compiled stuff")
            }
            PathType::Yaml => {
                anyhow::bail!(
                    "invalid cell build target [maybe you meant to add this as a bridge?]: {}",
                    og_path.display()
                );
            }
            _ => {
                anyhow::bail!("invalid build target: {}", og_path.display());
            }
        }
    }

    // Resolve instances: cell instances feed the instance map; bridge instances
    // parse their spec and bake the instance SRI as the bridge cell name.
    for instance in instances {
        match instance.reference()? {
            models::InstanceRef::Class(id) => {
                if !cell_ids.contains(&id) {
                    if bridge_specs.contains_key(&id) {
                        anyhow::bail!(
                            "instance `class: {id}` references a bridge class; use `bridge:`"
                        );
                    }
                    anyhow::bail!("instance references unknown cell class `{id}`");
                }

                let arguments = instance.init_arguments(folder)?;
                let restart = instance
                    .restart
                    .as_ref()
                    .map(models::RestartSpec::to_policy);
                let srn = instance.srn.unwrap_or_else(|| id.clone());
                info.instances.push(CellInstance {
                    id,
                    srn: Some(srn),
                    tags: instance.tags,
                    arguments,
                    restart,
                });
            }
            models::InstanceRef::Bridge(id) => {
                if instance.has_init() {
                    anyhow::bail!(
                        "instance `bridge: {id}` sets `init`/`init_file`, but init arguments \
                         only apply to cell instances"
                    );
                }
                let Some(spec) = bridge_specs.get(&id) else {
                    if cell_ids.contains(&id) {
                        anyhow::bail!(
                            "instance `bridge: {id}` references a cell class; use `class:`"
                        );
                    }
                    anyhow::bail!("instance references unknown bridge class `{id}`");
                };

                let srn = instance.srn.unwrap_or_else(|| id.clone());
                crate::info!(ctx, "Building {}...", srn);

                match crate::parse_from_file(spec)? {
                    models::BridgeInput::Mqtt(bridge) => {
                        info.mqtt_bridges.push(models::mqtt::convert(srn, bridge)?);
                    }
                    models::BridgeInput::Http(bridge) => {
                        info.http_bridges.push(models::http::convert(srn, bridge)?);
                    }
                }
            }
        }
    }

    Ok(info)
}

/// Maps the app-spec `platforms` field to CLI [`Platform`]s, defaulting to
/// [`Platform::DEFAULT`] when absent.
fn parse_platforms(spec: Option<models::PlatformSpec>) -> anyhow::Result<Vec<Platform>> {
    match spec {
        None => Ok(Platform::DEFAULT.to_vec()),
        Some(models::StringOr::String(spec)) => spec.split(',').map(Platform::from_str).collect(),
        Some(models::StringOr::Type(platforms)) => platforms.iter().map(|p| p.parse()).collect(),
    }
}

pub(crate) fn to_build_cargo_target(target: models::CargoTarget) -> myrmic_build::CargoTarget {
    match target {
        models::CargoTarget::Auto => myrmic_build::CargoTarget::Auto,
        models::CargoTarget::Lib => myrmic_build::CargoTarget::Lib,
        models::CargoTarget::Named(name) => myrmic_build::CargoTarget::Named(name),
    }
}

/// Builds the crate, or every member of the workspace, at `manifest_path`.
/// `runtime_name` only means something to a firmware crate, which bakes it in
/// as the device's name.
pub fn build_toml(
    ctx: Ctx,
    manifest_path: &Path,
    platforms: &[Platform],
    cargo_target: models::CargoTarget,
    runtime_name: Option<&str>,
) -> anyhow::Result<Vec<CellClass>> {
    let info = cargo::crate_info(manifest_path)?;
    let cargo_target = to_build_cargo_target(cargo_target);

    let members: Vec<&Path> = match info.as_root() {
        Some(ws) => {
            // A single selector can't span a workspace; each member resolves its
            // own target automatically (sole bin, else sole lib).
            if cargo_target != myrmic_build::CargoTarget::Auto {
                anyhow::bail!(
                    "`--target` selects a target within a single crate, but `{}` is a workspace; \
                     point the build path at a specific crate",
                    manifest_path.display(),
                );
            }
            ws.members.iter().map(PathBuf::as_path).collect()
        }
        None => vec![&info.manifest_path],
    };

    let mut classes = vec![];
    for member in &members {
        if let Some(cc) = build_member(ctx, member, platforms, &cargo_target, runtime_name)? {
            classes.push(cc);
        }
    }
    if runtime_name.is_some() && classes.len() == members.len() {
        crate::warn!(
            ctx,
            "--name was provided, but will be ignored: it names a firmware's runtime, and `{}` \
             builds no firmware",
            manifest_path.display()
        );
    }

    Ok(classes)
}

/// Builds one crate of a `myrmic build <path>` run. A firmware crate yields an
/// ELF and no cell class; anything else is a cell.
fn build_member(
    ctx: Ctx,
    path: &Path,
    platforms: &[Platform],
    cargo_target: &myrmic_build::CargoTarget,
    runtime_name: Option<&str>,
) -> anyhow::Result<Option<CellClass>> {
    match firmware::chip_of(path)? {
        Some(chip) => {
            build_firmware(ctx, path, chip, platforms, cargo_target, runtime_name)?;
            Ok(None)
        }
        None => build_cell(ctx, path, platforms, cargo_target),
    }
}

fn build_firmware(
    ctx: Ctx,
    path: &Path,
    chip: firmware::Chip,
    platforms: &[Platform],
    cargo_target: &myrmic_build::CargoTarget,
    runtime_name: Option<&str>,
) -> anyhow::Result<()> {
    if platforms != Platform::DEFAULT {
        crate::warn!(
            ctx,
            "--platform is ignored for a firmware crate; the chip ({chip}) comes from \
             `[package.metadata.myrmic] firmware`"
        );
    }
    report_building(ctx, chip, path, runtime_name);

    let built = firmware::build(path, cargo_target, None, runtime_name)?;
    report_layout(ctx, &built);

    crate::info!(ctx, "Firmware: {}", built.elf.display());
    if let Some(table) = &built.partition_table {
        crate::info!(ctx, "Partition table: {}", table.display());
    }
    crate::info!(
        ctx,
        "Flash with: myrmic flash {}",
        path.parent().unwrap_or(path).display()
    );

    Ok(())
}

/// Announces a firmware build, naming the runtime it is built for when one was
/// given.
pub(crate) fn report_building(
    ctx: Ctx,
    chip: firmware::Chip,
    manifest_path: &Path,
    runtime_name: Option<&str>,
) {
    match runtime_name {
        Some(name) => crate::info!(
            ctx,
            "Building {chip} firmware for runtime `{name}`: {}",
            manifest_path.display()
        ),
        None => crate::info!(ctx, "Building {chip} firmware: {}", manifest_path.display()),
    }
}

/// Logs where a firmware build's partition layout came from, and warns when
/// nothing tells espflash which table to flash it with.
pub(crate) fn report_layout(ctx: Ctx, built: &firmware::FirmwareBuild) {
    if let Some(partitions) = &built.default_partitions {
        crate::info!(
            ctx,
            "No partitions.toml beside the crate; using the default layout ({partitions})"
        );
    }
    if built.partition_table.is_none() {
        crate::warn!(
            ctx,
            "no espflash.toml beside the crate names a partition table; flashing will fall \
             back to espflash's default table and may write an oversized image"
        );
    }
}

/// Maps the CLI's build platforms to `myrmic-build` platforms
fn build_platforms(platforms: &[Platform]) -> Vec<myrmic_build::Platform> {
    platforms
        .iter()
        .map(|platform| match platform {
            Platform::Linux => myrmic_build::Platform::Linux,
            Platform::Riscv32imac => myrmic_build::Platform::Esp32c6,
        })
        .collect()
}

fn build_cell(
    ctx: Ctx,
    path: &Path,
    platforms: &[Platform],
    cargo_target: &myrmic_build::CargoTarget,
) -> anyhow::Result<Option<CellClass>> {
    let _folder = path
        .parent()
        .with_context(|| format!("unable to determine folder: {}", path.display()))?;

    let info = cargo::crate_info(path)?;

    let package_name = info.package_name.with_context(|| {
        format!(
            "`{}` resolves to a workspace root (no [package]); point the build path at a \
             specific crate, e.g. ./server",
            path.display()
        )
    })?;

    let mut wasm_path = None;
    let mut riscv32imac = None;

    crate::info!(ctx, "Attempting to build: {}", path.display());

    // Route every wasm/esp platform through the shared `myrmic-build`
    // pipeline, which compiles the wasm and (for esp platforms) AOT-compiles
    // it. The wasm artifact is identical across platforms; cargo skips the
    // redundant rebuilds.
    for platform in build_platforms(platforms) {
        let built = myrmic_build::build(path, platform, cargo_target)?;
        wasm_path = Some(built.wasm);
        match platform {
            myrmic_build::Platform::Esp32c5
            | myrmic_build::Platform::Esp32c6
            | myrmic_build::Platform::Esp32c61 => {
                riscv32imac = built.aot.map(|a| (a.aot, a.meta));
            }
            myrmic_build::Platform::Linux => {}
        }
    }

    if wasm_path.is_none() {
        anyhow::bail!("wasm build artifact wasn't generated, unable to continue");
    }

    if wasm_path.is_none() {
        crate::warn!(
            ctx,
            "{} produced no build artifacts, ignoring...",
            path.display()
        );
        return Ok(None);
    }

    // An explicitly named target becomes a class of its own, so several cells
    // can live in one crate as separate bins; otherwise the class is the package.
    let name = match cargo_target {
        myrmic_build::CargoTarget::Named(target) => target.clone(),
        myrmic_build::CargoTarget::Lib | myrmic_build::CargoTarget::Auto => package_name,
    };

    Ok(Some(CellClass {
        name,
        wasm_path,
        riscv32imac,
    }))
}
