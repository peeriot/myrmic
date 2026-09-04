use crate::args::Ctx;
use anyhow::Context;

#[derive(clap::Parser)]
pub struct Delete {
    name: String,
}

pub async fn handle(ctx: Ctx, cmd: Delete) -> anyhow::Result<()> {
    let Delete { name } = cmd;

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    client
        .remove_class(&name)
        .await
        .with_context(|| format!("unable to remove class: {}", name))?;

    Ok(())
}
