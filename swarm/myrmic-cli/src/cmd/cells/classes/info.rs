use anyhow::Context;
use cell_protocol::BlobHash;

use crate::args::Ctx;
use crate::render::{class_label, names_by_class};

#[derive(clap::Parser)]
pub struct Info {
    name: String,
}

pub async fn handle(ctx: Ctx, cmd: Info) -> anyhow::Result<()> {
    let Info { name } = cmd;

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let class = client
        .resolve_class_name(&name)
        .await
        .with_context(|| format!("unable to look up class '{name}'"))?;

    match class {
        Some(class) => print_class(&client, &name, class).await?,
        None => println!("Class '{name}' is not registered"),
    }

    Ok(())
}

async fn print_class(
    client: &sorg_client::Client,
    name: &str,
    class: BlobHash,
) -> anyhow::Result<()> {
    let info = client
        .get_class_info(class)
        .await
        .with_context(|| format!("unable to fetch info for class '{name}'"))?
        .with_context(|| {
            format!(
                "name '{name}' points to unregistered class {}",
                class.to_hex()
            )
        })?;
    let names = names_by_class(client.list_class_names().await?);

    println!("Class {}", class_label(class, &names));
    println!("  id:   {}", class.to_hex());
    println!("  kind: {}", super::describe(&info, &names));

    Ok(())
}
