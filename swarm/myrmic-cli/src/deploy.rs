use std::collections::HashMap;

use sorg_common::{HttpBridgeApi, MqttBridge, RequirementTags, RestartPolicy};

use crate::args::Ctx;
use crate::{build, models, nest};

use crate::platforms::Platform;
use anyhow::Context as _;

/// The per-root knobs a direct cell deploy carries: the `#[init]` argument
/// buffer and the restart policy. Both only apply to cells the CLI deploys as
/// roots, so a multi-class crate hands `arguments` to its sole class while
/// every class gets the same `restart`.
#[derive(Debug, Default, Clone)]
pub struct RootConfig {
    pub arguments: Option<Vec<u8>>,
    pub restart: RestartPolicy,
}

pub async fn deploy_http_bridge(
    ctx: Ctx,
    session: &zenoh::Session,
    bridges: &[HttpBridgeApi],
    tags: RequirementTags,
) -> anyhow::Result<()> {
    let tags = ensure_linux_tag(tags);
    let sorg = ctx.sorg(session.clone());
    for api in bridges {
        let sri = cell_protocol::Sri::of_path(&api.cell_name)
            .map_err(|e| anyhow::anyhow!("invalid bridge name '{}': {e}", api.cell_name))?;
        sorg.deploy_http_bridge(sri, api.clone(), tags.clone())
            .await
            .with_context(|| format!("unable to deploy http bridge '{}'", api.cell_name))?;
        crate::info!(ctx, "deployed {} (sri {sri})", api.cell_name);
    }
    Ok(())
}

pub async fn deploy_mqtt_bridge(
    ctx: Ctx,
    session: &zenoh::Session,
    bridges: &[MqttBridge],
    tags: RequirementTags,
) -> anyhow::Result<()> {
    let tags = ensure_linux_tag(tags);
    let sorg = ctx.sorg(session.clone());
    for bridge in bridges {
        let sri = cell_protocol::Sri::of_path(&bridge.cell_name)
            .map_err(|e| anyhow::anyhow!("invalid bridge name '{}': {e}", bridge.cell_name))?;
        sorg.deploy_mqtt_bridge(sri, bridge.clone(), tags.clone())
            .await
            .with_context(|| format!("unable to deploy mqtt bridge '{}'", bridge.cell_name))?;
        crate::info!(ctx, "deployed {} (sri {sri})", bridge.cell_name);
    }
    Ok(())
}

pub async fn deploy_toml(
    ctx: Ctx,
    name: Option<String>,
    tags: RequirementTags,
    path: &std::path::Path,
    platforms: &[Platform],
    cargo_target: models::CargoTarget,
    mut root: RootConfig,
) -> anyhow::Result<()> {
    let classes = build::build_toml(ctx, path, platforms, cargo_target)?;

    let session = ctx.session().await?;

    let multi = classes.len() > 1;

    if root.arguments.is_some() && multi {
        anyhow::bail!(
            "--init/--init-file cannot target a multi-class crate; deploy a single class instead"
        );
    }

    // Resolve embedded spawn references (against sibling classes built here and
    // the class registry) and repoint each class at its patched wasm, exactly
    // like the app path — a lone crate/`.wasm` deploy used to skip this.
    let mut classes: HashMap<String, build::CellClass> =
        classes.into_iter().map(|c| (c.name.clone(), c)).collect();
    crate::spawn_patch::patch_spawn_refs(ctx, &session, &mut classes).await?;

    // Only a single-class deploy can carry init arguments (guarded above), so
    // `take` hands the buffer to the sole class and leaves `None` otherwise.
    for class in classes.into_values() {
        if class.wasm_path.is_none() {
            crate::warn!(ctx, "{} produced no build artifact, skipping.", class.name);
            continue;
        }

        let alloc;
        let sri = match name.as_deref() {
            Some(sri) if multi => {
                alloc = format!("{}{}", sri, class.name);
                &*alloc
            }
            Some(sri) => sri,
            None => &*class.name,
        };

        let class_root = RootConfig {
            arguments: root.arguments.take(),
            restart: root.restart.clone(),
        };
        deploy_cell(ctx, &session, sri, &class, tags.clone(), class_root).await?;
    }

    Ok(())
}

pub async fn deploy_app(
    ctx: Ctx,
    path: &std::path::Path,
    app: models::App,
    restart: Option<RestartPolicy>,
) -> anyhow::Result<()> {
    let info = build::build_app(ctx, path, app, None)?;

    deploy_app_info(ctx, info, restart).await
}

/// Renders a policy the way `--policy` and an app spec spell it. `--policy`
/// resets the crash-loop bounds too, so they are spelled out alongside the
/// trigger — otherwise an override that only moves the bounds reads as a no-op.
fn describe_restart(policy: &RestartPolicy) -> String {
    let trigger = match policy.restart_type {
        sorg_common::RestartType::Never => "never",
        sorg_common::RestartType::OnError => "on-error",
        sorg_common::RestartType::Always => "always",
    };
    format!(
        "{trigger} (max {}, window {}ms, delay {}ms)",
        policy.max_restarts, policy.window_ms, policy.delay_ms
    )
}

/// Replaces every instance's restart policy with `restart`, reporting each
/// instance whose app spec asked for something else.
fn override_restart(ctx: Ctx, info: &mut build::AppInfo, restart: &RestartPolicy) {
    for instance in &mut info.instances {
        if let Some(declared) = &instance.restart
            && declared != restart
        {
            crate::warn!(
                ctx,
                "--policy overrides the restart policy declared for '{}': {} -> {}",
                instance.srn.as_deref().unwrap_or(&instance.id),
                describe_restart(declared),
                describe_restart(restart)
            );
        }
        instance.restart = Some(restart.clone());
    }
}

async fn deploy_app_info(
    ctx: Ctx,
    mut info: build::AppInfo,
    restart: Option<RestartPolicy>,
) -> anyhow::Result<()> {
    if let Some(restart) = &restart {
        override_restart(ctx, &mut info, restart);
    }

    let mut errors = false;
    for instance in &info.instances {
        // just double check everything is referenced properly.
        let Some(class) = info.classes.get(&instance.id) else {
            errors = true;
            crate::error!(ctx, "no cell class with id: {}", instance.id);
            continue;
        };
        // if there's nothing to deploy, then we can't do much...
        if class.wasm_path.is_none() {
            errors = true;
            crate::error!(
                ctx,
                "cell class has no build artifact: {} [we only support linux]",
                instance.id
            );
            continue;
        }
    }
    if errors {
        anyhow::bail!("validation errors during app deployment [see above]");
    }

    let session = ctx.session().await?;

    crate::spawn_patch::patch_spawn_refs(ctx, &session, &mut info.classes).await?;

    let sorg = ctx.sorg(session.clone());

    for class in info.classes.values() {
        upload_class_artifacts(ctx, &sorg, &class.name, class).await?;
    }

    let request = build_deploy_request(&info)?;
    if request.cells.is_empty() {
        crate::warn!(ctx, "no cells to deploy; '{}' not deployed", info.name);
    } else {
        sorg.deploy_cells(request)
            .await
            .context("application deployment failed")?;
    }

    Ok(())
}

pub async fn deploy_nest(
    ctx: Ctx,
    path: &std::path::Path,
    restart: Option<RestartPolicy>,
) -> anyhow::Result<()> {
    let info = nest::read(ctx, path)?;
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        && stem != info.name
    {
        crate::warn!(
            ctx,
            "nest file name '{}' differs from the baked app name '{}'",
            stem,
            info.name
        );
    }
    deploy_app_info(ctx, info, restart).await
}

/// Uploads everything a class build produced — the wasm plus any AOT pair — and
/// registers it under `class_name`, which differs from `class.name` when a
/// single-cell deploy registers the class under the SRN it was given.
async fn upload_class_artifacts(
    ctx: Ctx,
    sorg: &sorg_client::Client,
    class_name: &str,
    class: &build::CellClass,
) -> anyhow::Result<()> {
    use cell_protocol::{AddMode, ArtifactPlatform, ClassArtifact};

    if let Some(wasm_path) = &class.wasm_path {
        let binary = std::fs::read(wasm_path)
            .with_context(|| format!("unable to read {}", wasm_path.display()))?;
        sorg.add_class_artifact(class_name, ClassArtifact::Wasm(binary), AddMode::Force)
            .await
            .with_context(|| format!("unable to add wasm for class '{class_name}'"))?;
        crate::debug!(ctx, "uploaded wasm for class '{class_name}'");
    }

    if let Some((aot_path, meta_path)) = &class.riscv32imac {
        let aot_blob = std::fs::read(aot_path)
            .with_context(|| format!("unable to read {}", aot_path.display()))?;
        let meta_blob = std::fs::read(meta_path)
            .with_context(|| format!("unable to read {}", meta_path.display()))?;
        sorg.add_class_artifact(
            class_name,
            ClassArtifact::Aot {
                platform: ArtifactPlatform::Riscv32imac,
                aot_blob,
                meta_blob,
            },
            AddMode::Force,
        )
        .await
        .with_context(|| format!("unable to add esp32c6 artifacts for class '{class_name}'"))?;
        crate::debug!(ctx, "uploaded esp32c6 artifacts for class '{class_name}'");
    }

    Ok(())
}

/// Deploys a standalone `.wasm` file as a single cell, resolving any spawn
/// references it embeds against the class registry before upload.
pub async fn deploy_wasm(
    ctx: Ctx,
    session: &zenoh::Session,
    sri: &str,
    path: &std::path::Path,
    tags: RequirementTags,
    root: RootConfig,
) -> anyhow::Result<()> {
    let mut classes = HashMap::from([(
        sri.to_owned(),
        build::CellClass {
            name: sri.to_owned(),
            wasm_path: Some(path.to_path_buf()),
            riscv32imac: None,
        },
    )]);

    crate::spawn_patch::patch_spawn_refs(ctx, session, &mut classes).await?;

    deploy_cell(ctx, session, sri, &classes[sri], tags, root).await
}

pub async fn deploy_cell(
    mut ctx: Ctx,
    session: &zenoh::Session,
    sri: &str,
    class: &build::CellClass,
    tags: RequirementTags,
    root: RootConfig,
) -> anyhow::Result<()> {
    if class.wasm_path.is_none() {
        anyhow::bail!("no wasm build artifact for cell '{sri}', unable to deploy");
    }

    // Extend the default timeout when attempting to deploy to non-linux systems.
    // (They typically take their sweet time...)
    let ctx = {
        let build::CellClass {
            name: _,
            wasm_path: _,
            riscv32imac,
        } = class;

        if riscv32imac.is_some() && ctx.timeout.is_none() {
            ctx.timeout = Some(std::time::Duration::from_mins(1).into());
            ctx
        } else {
            ctx
        }
    };

    let sorg = ctx.sorg(session.clone());

    // `sri` here is the human-readable SRN. The class keeps that name; the
    // instance's address is the SRI deterministically derived from it, so the
    // deployed cell is addressable by UUID (and, via resolution, by the SRN).
    let cell_sri = cell_protocol::Sri::of_path(sri)
        .map_err(|e| anyhow::anyhow!("invalid cell name '{sri}': {e}"))?;

    upload_class_artifacts(ctx, &sorg, sri, class).await?;

    crate::info!(ctx, "deploying cell (srn = {sri}, sri = {cell_sri})");

    // A standalone cell is the root of its own app, named after its SRN.
    let cell = sorg_common::CellDeployment::new(
        cell_sri,
        sorg_common::CellConfig::Wasm {
            class: sri.to_owned(),
        },
    )
    .with_tags(tags)
    .with_arguments(root.arguments)
    .with_app(Some(sri.to_owned()))
    .with_restart(root.restart);

    sorg.deploy_cells(sorg_common::DeployRequest::new(vec![cell]))
        .await
        .context("unable to deploy cell")?;

    crate::info!(ctx, "deployed cell (srn = {sri}, sri = {cell_sri})");

    Ok(())
}

pub fn build_deploy_request(info: &build::AppInfo) -> anyhow::Result<sorg_common::DeployRequest> {
    // App cells are addressed by the SRI derived from their SRN, exactly like a
    // single-cell deploy — so the same name reaches the same cell whether it is
    // deployed alone or as part of an app. Every cell carries the app name, so
    // the whole bundle (and anything it later spawns) groups under it.
    let app = info.name.clone();
    let derive = |srn: &str| -> anyhow::Result<cell_protocol::Sri> {
        cell_protocol::Sri::of_path(srn)
            .map_err(|e| anyhow::anyhow!("invalid cell name '{srn}': {e}"))
    };

    let mut cells: Vec<sorg_common::CellDeployment> = info
        .instances
        .iter()
        .map(|instance| {
            let class = &info.classes[&instance.id];
            let srn = instance.srn.as_deref().unwrap_or(&instance.id);
            let cell = sorg_common::CellDeployment::new(
                derive(srn)?,
                sorg_common::CellConfig::Wasm {
                    class: class.name.clone(),
                },
            );
            let cell = if instance.tags.is_empty() {
                cell
            } else {
                cell.with_tags(RequirementTags::new(instance.tags.clone()))
            };
            Ok(cell
                .with_arguments(instance.arguments.clone())
                .with_app(Some(app.clone()))
                .with_restart(instance.restart.clone().unwrap_or_default()))
        })
        .collect::<anyhow::Result<_>>()?;

    let linux_tag = RequirementTags::new(vec![myrmic_tags::TAG_LINUX]);

    for bridge in &info.mqtt_bridges {
        cells.push(
            sorg_common::CellDeployment::new(
                derive(&bridge.cell_name)?,
                sorg_common::CellConfig::MqttBridge(bridge.clone()),
            )
            .with_tags(linux_tag.clone())
            .with_app(Some(app.clone())),
        );
    }

    for api in &info.http_bridges {
        cells.push(
            sorg_common::CellDeployment::new(
                derive(&api.cell_name)?,
                sorg_common::CellConfig::HttpBridge(api.clone()),
            )
            .with_tags(linux_tag.clone())
            .with_app(Some(app.clone())),
        );
    }

    Ok(sorg_common::DeployRequest::new(cells))
}

fn ensure_linux_tag(tags: RequirementTags) -> RequirementTags {
    let has_linux_tag = tags
        .as_ref()
        .iter()
        .any(|t| t.as_ref() == myrmic_tags::TAG_LINUX);

    if has_linux_tag {
        return tags;
    }
    let mut all: Vec<String> = tags
        .as_ref()
        .iter()
        .map(|t| t.as_ref().to_owned())
        .collect();
    all.push(myrmic_tags::TAG_LINUX.to_owned());
    RequirementTags::new(all)
}

#[cfg(test)]
mod tests;
