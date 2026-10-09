use crate::args::Ctx;
use crate::render::{class_label, names_by_class};

#[derive(clap::Parser, Default)]
pub struct List {}

pub async fn handle(ctx: Ctx, _cmd: List) -> anyhow::Result<()> {
    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let classes = client.list_classes().await?;
    if classes.is_empty() {
        println!("No classes registered");
        return Ok(());
    }
    let names = names_by_class(client.list_class_names().await?);

    for (class, info) in &classes {
        println!("{} [{}]", class_label(*class, &names), class.to_hex());
        println!("  {}", super::describe(info, &names));
    }

    Ok(())
}
