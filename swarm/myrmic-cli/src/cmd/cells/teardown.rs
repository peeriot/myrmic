use crate::args::Ctx;
use crate::render::names_by_class;
use anyhow::Context;
use myrmic_common::cells::Sri;
use sorg_common::FenceOutcome;

#[derive(clap::Parser)]
pub struct Teardown {
    /// The SRI or SRN of the cell to tear down.
    #[clap(value_name = "SRI/SRN")]
    pub id: String,

    /// Also remove the cell's class name (a root's SRN) and, once nothing else
    /// references it, the class
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

    let class = instance.class;

    // Undeploy erases the instance row itself; this sweeps the corpse left
    // behind if that best-effort erase failed. Fenced by the generation read
    // above, so a cell already running again under the SRI keeps its row.
    match client
        .erase_instance(&target, instance.gen_id)
        .await
        .with_context(|| format!("failed to erase instance '{id}'"))?
    {
        FenceOutcome::Applied | FenceOutcome::Absent => {}
        FenceOutcome::Superseded { current } => {
            println!("Instance row kept: '{id}' is already running again as generation {current}");
        }
    }

    if remove_class {
        // A root's deploy named its class after the root's SRN; that label is
        // this cell's to drop. Any other name belongs to another deploy.
        if instance.lineage.parent.is_none()
            && let Some(srn) = &instance.lineage.local_name
            && client.unbind_class_name(srn, class).await?
        {
            println!("Name '{srn}' removed");
        }
        let removal = client
            .remove_class_if_unreferenced(class)
            .await
            .with_context(|| format!("failed to remove class {}", class.to_hex()))?;
        let names = names_by_class(client.list_class_names().await?);
        super::classes::print_removal(class, &removal, &names);
    }

    println!("Cell '{id}' torn down");

    Ok(())
}
