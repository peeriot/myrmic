use crate::args::Ctx;
use crate::render::names_by_class;
use anyhow::Context;
use cell_protocol::{BlobHash, NATIVE_FIRMWARE_CLASS};

/// Remove a class name, and the class once nothing references it; with
/// `--ref-is-hash`, remove a class by its hash
#[derive(clap::Parser)]
pub struct Delete {
    /// The class's name, or with `--ref-is-hash` its hash or a unique prefix of
    /// it
    class_ref: String,

    /// Treat the argument as a class hash (or a unique prefix of one)
    #[clap(long, short)]
    ref_is_hash: bool,
}

pub async fn handle(ctx: Ctx, cmd: Delete) -> anyhow::Result<()> {
    let Delete {
        class_ref,
        ref_is_hash,
    } = cmd;

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let class = if ref_is_hash {
        let prefix = class_ref.to_lowercase();
        anyhow::ensure!(
            prefix != NATIVE_FIRMWARE_CLASS.to_hex(),
            "'{class_ref}' is the class shown as <firmware>: native firmware cells run code built \
             into the firmware, which is not a registered class and cannot be removed"
        );
        let matches: Vec<BlobHash> = client
            .list_classes()
            .await
            .context("unable to list classes")?
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| id.to_hex().starts_with(&prefix))
            .collect();
        match matches.as_slice() {
            [id] => *id,
            [] => anyhow::bail!("no class with hash prefix '{class_ref}'"),
            _ => anyhow::bail!(
                "hash prefix '{class_ref}' matches {} classes; nothing removed",
                matches.len()
            ),
        }
    } else {
        let class = client
            .resolve_class_name(&class_ref)
            .await
            .with_context(|| format!("unable to look up class '{class_ref}'"))?
            .with_context(|| format!("class '{class_ref}' not found"))?;

        // Only the label this lookup saw: a name moved meanwhile is someone else's.
        if !client.unbind_class_name(&class_ref, class).await? {
            anyhow::bail!("class '{class_ref}' changed while removing it; nothing removed");
        }
        println!("Name '{class_ref}' removed");
        class
    };

    let removal = client
        .remove_class_if_unreferenced(class)
        .await
        .with_context(|| format!("unable to remove class: {class_ref}"))?;
    let names = names_by_class(client.list_class_names().await?);
    super::print_removal(class, &removal, &names);

    Ok(())
}
