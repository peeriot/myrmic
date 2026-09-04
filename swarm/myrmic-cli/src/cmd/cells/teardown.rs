use crate::args::Ctx;
use anyhow::Context;
use myrmic_common::cells::Sri;

#[derive(clap::Parser)]
pub struct Teardown {
    /// The SRI or SRN of the cell to tear down.
    #[clap(value_name = "SRI/SRN")]
    pub id: String,

    /// Also remove the cell class from the datalayer
    #[arg(long)]
    pub remove_class: bool,
}

pub async fn handle(ctx: Ctx, cmd: Teardown) -> anyhow::Result<()> {
    let Teardown { id, remove_class } = cmd;

    let target =
        Sri::from_target(&id).map_err(|e| anyhow::anyhow!("invalid target '{id}': {e}"))?;

    let session = ctx.session().await?;
    let client = ctx.sorg(session);

    let instance = client
        .inspect_instance(&target)
        .await
        .with_context(|| format!("no instance found for '{id}'"))?;

    if !client.placement_exists(&target).await? {
        anyhow::bail!("cell '{id}' is not deployed");
    }

    client
        .undeploy_cell(target)
        .await
        .with_context(|| format!("failed to undeploy cell '{id}'"))?;

    let class_name = instance.class_name;

    // Undeploy erases the instance row itself; this sweeps the corpse left
    // behind if that best-effort erase failed.
    client
        .erase_instance_if_present(&target)
        .await
        .with_context(|| format!("failed to erase instance '{id}'"))?;

    if remove_class {
        let instances = client.list_instances().await?;
        let class_still_used = instances.iter().any(|i| i.class_name == class_name);
        if class_still_used {
            println!("Class '{class_name}' kept (other instances still reference it)");
        } else {
            client
                .remove_class(&class_name)
                .await
                .with_context(|| format!("failed to remove class '{class_name}'"))?;
        }
    }

    println!("Cell '{id}' torn down");

    Ok(())
}
