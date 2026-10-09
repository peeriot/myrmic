use crate::args::Ctx;
use anyhow::Context;
use cell_protocol::ArtifactPlatform;
use std::path::PathBuf;

/// Register a cell class under a name, or add an AOT binary under it
#[derive(clap::Parser)]
pub struct Add {
    /// Name for the cell class. Binding it to a class moves it there, if it
    /// pointed to another one.
    name: String,

    /// Path to the .wasm file containing the class binary
    #[arg(long)]
    wasm: Option<PathBuf>,

    /// Path to an AOT-compiled binary: added to the wasm class the name points
    /// to, otherwise registered as a class of its own
    #[arg(long, requires = "platform")]
    aot: Option<PathBuf>,

    /// Target architecture (e.g. esp32c6; required with --aot)
    #[arg(long, requires = "aot")]
    platform: Option<ArtifactPlatform>,
}

pub async fn handle(ctx: Ctx, cmd: Add) -> anyhow::Result<()> {
    let Add {
        name,
        wasm,
        aot,
        platform,
    } = cmd;

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    match (wasm, aot) {
        (Some(wasm), None) => {
            let binary = std::fs::read(&wasm)
                .with_context(|| format!("failed to read {}", wasm.display()))?;
            let id = client.add_class(&name, binary, vec![]).await?;
            println!("Class '{name}' is {}", id.to_hex());
        }
        (None, Some(aot)) => {
            let platform = platform.expect("clap ensures --platform is present");
            let binary =
                std::fs::read(&aot).with_context(|| format!("failed to read {}", aot.display()))?;
            let id = client.add_class_aot(&name, platform, binary).await?;
            println!(
                "AOT binary {} for '{platform}' added under '{name}'",
                id.to_hex()
            );
        }
        (Some(_), Some(_)) => anyhow::bail!("--wasm and --aot are mutually exclusive"),
        (None, None) => anyhow::bail!("provide either --wasm or --aot/--platform"),
    }

    Ok(())
}
