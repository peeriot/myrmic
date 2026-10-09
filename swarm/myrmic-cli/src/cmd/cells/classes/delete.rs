use crate::args::Ctx;
use crate::render::names_by_class;
use anyhow::Context;

/// Remove a class name, and the class it pointed to once nothing references it
#[derive(clap::Parser)]
pub struct Delete {
    name: String,
}

pub async fn handle(ctx: Ctx, cmd: Delete) -> anyhow::Result<()> {
    let Delete { name } = cmd;

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let class = client
        .resolve_class_name(&name)
        .await
        .with_context(|| format!("unable to look up class '{name}'"))?
        .with_context(|| format!("class '{name}' not found"))?;
    // Only the label this lookup saw: a name moved meanwhile is someone else's.
    if !client.unbind_class_name(&name, class).await? {
        anyhow::bail!("class '{name}' changed while removing it; nothing removed");
    }
    println!("Name '{name}' removed");

    let removal = client
        .remove_class_if_unreferenced(class)
        .await
        .with_context(|| format!("unable to remove class: {name}"))?;
    let names = names_by_class(client.list_class_names().await?);
    super::print_removal(class, &removal, &names);

    Ok(())
}
