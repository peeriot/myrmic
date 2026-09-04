use anyhow::Context as _;

use crate::args::Ctx;
use crate::models::{self, DeployInput, http, mqtt};
use crate::platforms::Platform;
use crate::utils::{PathType, determine_wd};
use crate::{deploy, determine_name};

#[derive(clap::Parser)]
pub struct Deploy {
    /// The location of the cell that will be deployed.
    /// Defaults to the current working directory if none provided.
    path: Option<std::path::PathBuf>,

    /// The SRN (name) to deploy the cell under; the cell's SRI is derived
    /// from it. Defaults to the crate or wasm file name.
    #[clap(long, alias = "srn", value_name = "SRN")]
    name: Option<String>,

    /// Comma-separated list of build platforms to compile for (e.g. `linux`).
    #[clap(long)]
    platform: Option<String>,

    /// Which cargo target to build: `lib` or a target name. Omit to auto-select (sole bin, else sole lib).
    #[clap(long)]
    target: Option<models::CargoTarget>,

    /// Init arguments delivered to the cell's `#[init]` on deploy. Parsed like
    /// a `send` payload (JSON by default; a value that isn't valid JSON is sent
    /// as a JSON string). Single-cell (`.wasm` / crate) deploys only.
    #[clap(long, conflicts_with = "init_file")]
    init: Option<String>,

    /// File whose raw bytes are delivered verbatim as the cell's `#[init]`
    /// arguments. Single-cell (`.wasm` / crate) deploys only.
    #[clap(long)]
    init_file: Option<std::path::PathBuf>,

    /// Placement requirement tag for the cell. Repeatable.
    ///
    /// Tags are only passed as CLI arguments for standalone cell deployments.
    /// For app deployments, specify tags per cell in the app-spec YAML.
    #[clap(long = "tag", short = 't')]
    tags: Vec<String>,

    /// Restart policy for the deployed root cell(s): `never` (the default),
    /// `on-error` (also spelled `onerror`), or `always`. Crash-loop bounds keep
    /// their defaults. On an app deploy this overrides the `restart` declared
    /// in the app-spec YAML; bridges have no restart policy.
    #[clap(long, value_name = "POLICY")]
    policy: Option<models::RestartTypeName>,
}

pub async fn handle(ctx: Ctx, cmd: Deploy) -> anyhow::Result<()> {
    let Deploy {
        path,
        name,
        platform,
        target,
        init,
        init_file,
        tags,
        policy,
    } = cmd;

    if let Some(name) = name.as_deref()
        && name.parse::<uuid::Uuid>().is_ok()
    {
        anyhow::bail!(
            "--name expects an SRN (name), but '{name}' is an SRI (UUID); \
             the cell's SRI is derived from the name"
        );
    }

    let path = determine_wd(ctx, path)?;
    let tags = sorg_common::RequirementTags::new(tags);
    let init = resolve_init(init, init_file)?;
    let restart = policy.map(models::RestartTypeName::to_policy);

    let resolved = PathType::from_path(&path)?;
    if target.is_some() && !matches!(resolved.1, PathType::Toml) {
        crate::warn!(
            ctx,
            "--target was provided, but only applies to a crate build; ignoring"
        );
    }

    if platform.is_some() && !matches!(resolved.1, PathType::Toml) {
        crate::warn!(
            ctx,
            "--platform was provided, but only applies to a crate build; ignoring"
        );
    }

    if init.is_some() && !matches!(resolved.1, PathType::Toml | PathType::Wasm) {
        anyhow::bail!("--init/--init-file only apply to single-cell (.wasm or crate) deploys");
    }

    match resolved {
        (path, PathType::Yaml) => deploy_yaml(ctx, &path, name, tags, restart).await?,
        (path, PathType::Toml) => {
            let cargo_target = target.unwrap_or(models::CargoTarget::Auto);
            let platforms = Platform::parse_list(platform.as_deref())?;
            deploy::deploy_toml(
                ctx,
                name,
                tags,
                path.as_ref(),
                &platforms,
                cargo_target,
                root_config(init, restart),
            )
            .await?;
        }
        (path, PathType::Nest) => {
            if name.is_some() {
                crate::warn!(ctx, "--name was provided, but will be ignored");
            }
            deploy::deploy_nest(ctx, path.as_ref(), restart).await?;
        }
        (path, PathType::Wasm) => {
            let name = determine_name(name.as_deref(), &path)?;

            let session = ctx.session().await?;

            deploy::deploy_wasm(ctx, &session, name, &path, tags, root_config(init, restart))
                .await?;
        }
    }

    Ok(())
}

/// Deploys a `.yml` target: an app spec, or one of the bridge configuration
/// forms. Only the app spec carries a restart policy.
async fn deploy_yaml(
    ctx: Ctx,
    path: &std::path::Path,
    name: Option<String>,
    tags: sorg_common::RequirementTags,
    restart: Option<sorg_common::RestartPolicy>,
) -> anyhow::Result<()> {
    let input: DeployInput = crate::parse_from_file(path)?;

    if restart.is_some() && !input.carries_restart_policy() {
        crate::warn!(
            ctx,
            "--policy was provided, but bridges have no restart policy; ignoring"
        );
    }

    match input {
        DeployInput::App(app) => {
            if name.is_some() {
                crate::warn!(ctx, "--name was provided, but will be ignored");
            }

            deploy::deploy_app(ctx, path, app, restart).await
        }
        DeployInput::Mqtt(mut bridge) => {
            let name = name.unwrap_or_else(|| std::mem::take(&mut bridge.name));
            let bridge = mqtt::convert(name, bridge)?;

            let session = ctx.session().await?;

            deploy::deploy_mqtt_bridge(ctx, &session, &[bridge], tags).await
        }
        DeployInput::Http(mut api) => {
            let name = name.unwrap_or_else(|| std::mem::take(&mut api.name));
            let api = http::convert(name, api)?;

            let session = ctx.session().await?;

            deploy::deploy_http_bridge(ctx, &session, &[api], tags).await
        }
        DeployInput::MqttNest(config) => {
            if name.is_some() {
                crate::warn!(ctx, "--name was provided, but will be ignored");
            }

            let session = ctx.session().await?;

            deploy::deploy_mqtt_bridge(ctx, &session, &config.bridges, tags).await
        }
        DeployInput::HttpEgressNest(config) => {
            if name.is_some() {
                crate::warn!(ctx, "--name was provided, but will be ignored");
            }

            let session = ctx.session().await?;

            deploy::deploy_http_bridge(ctx, &session, &config.api, tags).await
        }
    }
}

/// Bundles the knobs a directly-deployed root carries. An omitted `--policy`
/// leaves the cell on [`RestartPolicy::default`](sorg_common::RestartPolicy)
/// (`Never`).
fn root_config(
    arguments: Option<Vec<u8>>,
    restart: Option<sorg_common::RestartPolicy>,
) -> deploy::RootConfig {
    deploy::RootConfig {
        arguments,
        restart: restart.unwrap_or_default(),
    }
}

/// Resolves the init-argument buffer from the mutually-exclusive `--init`
/// (payload literal, encoded like a `send` payload) / `--init-file` (raw bytes)
/// flags. The bytes are forwarded verbatim; the cell's `#[init]` decodes them.
fn resolve_init(
    init: Option<String>,
    init_file: Option<std::path::PathBuf>,
) -> anyhow::Result<Option<Vec<u8>>> {
    match (init, init_file) {
        (Some(_), Some(_)) => anyhow::bail!("--init and --init-file are mutually exclusive"),
        (Some(payload), None) => Ok(Some(crate::payload::encode(payload, false)?)),
        (None, Some(path)) => {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("failed to read init file '{}'", path.display()))?;
            Ok(Some(bytes))
        }
        (None, None) => Ok(None),
    }
}
