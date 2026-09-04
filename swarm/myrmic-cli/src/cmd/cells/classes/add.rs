use crate::args::Ctx;
use anyhow::Context;
use cell_protocol::{AddMode, ArtifactPlatform, ClassArtifact};
use std::path::PathBuf;

/// Add an artifact to a cell class (creates the class if it doesn't exist)
#[derive(clap::Parser)]
pub struct Add {
    /// Name for the cell class
    name: String,

    /// Path to the .wasm file containing the class binary
    #[arg(long)]
    wasm: Option<PathBuf>,

    /// Path to the AOT-compiled binary
    #[arg(long, requires_all = ["platform", "meta"])]
    aot: Option<PathBuf>,

    /// Path to the metadata file (required with --aot)
    #[arg(long, requires_all = ["platform", "aot"])]
    meta: Option<PathBuf>,

    /// Target architecture (e.g. esp32c6; required with --aot)
    #[arg(long, requires_all = ["aot", "meta"])]
    platform: Option<ArtifactPlatform>,

    /// Overwrite an existing class with the same name or binary
    #[arg(long)]
    force: bool,
}

pub async fn handle(ctx: Ctx, cmd: Add) -> anyhow::Result<()> {
    let Add {
        name,
        wasm,
        aot,
        meta,
        platform,
        force,
    } = cmd;

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let mode = if force {
        AddMode::Force
    } else {
        AddMode::Strict
    };

    let artifact = match (wasm, aot) {
        (Some(wasm), None) => {
            let binary = std::fs::read(&wasm)
                .with_context(|| format!("failed to read {}", wasm.display()))?;
            ClassArtifact::Wasm(binary)
        }
        (None, Some(aot_path)) => {
            let platform = platform.expect("clap ensures --platform is present");
            let meta_path = meta.expect("clap ensures --meta is present");
            let aot_blob = std::fs::read(&aot_path)
                .with_context(|| format!("failed to read {}", aot_path.display()))?;
            let meta_blob = std::fs::read(&meta_path)
                .with_context(|| format!("failed to read {}", meta_path.display()))?;
            ClassArtifact::Aot {
                platform,
                aot_blob,
                meta_blob,
            }
        }
        (Some(_), Some(_)) => anyhow::bail!("--wasm and --aot are mutually exclusive"),
        (None, None) => anyhow::bail!("provide either --wasm or --aot/--meta/--target"),
    };

    let info = client.add_class_artifact(&name, artifact, mode).await?;

    let hash = info
        .wasm_hash
        .as_ref()
        .map_or("none".to_owned(), |h| h.to_hex());

    println!("Class '{}' updated (wasm hash: {})", info.name, hash);

    for a in &info.artifacts {
        println!(
            "  target '{}' (aot: [{}], meta: [{}])",
            a.platform,
            a.aot_hash.to_hex(),
            a.meta_hash.to_hex()
        );
    }

    Ok(())
}
