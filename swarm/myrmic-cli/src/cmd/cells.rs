use crate::args::Ctx;

mod classes;
mod status;
mod teardown;

#[derive(clap::Parser)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Cells {
    #[clap(subcommand)]
    cmd: Option<Cmd>,

    // `status` is the default, so its arguments are accepted here directly.
    #[clap(flatten)]
    status: status::Status,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Manage cell classes.
    ///
    /// Subcommands add, remove, and inspect cell classes. With no subcommand,
    /// lists registered classes.
    #[clap(alias = "class")]
    Classes(classes::Classes),
    /// Show the status of deployed cells.
    ///
    /// Given one or more SRIs or SRNs, renders each match with its whole spawn
    /// subtree. With no target, lists all registered cells.
    Status(status::Status),
    /// Tear down a deployed cell.
    ///
    /// Forcibly undeploys the cell addressed by the given SRI or SRN,
    /// optionally removing its class from the datalayer.
    Teardown(teardown::Teardown),
}

pub async fn handle(ctx: Ctx, cmd: Cells) -> anyhow::Result<()> {
    let cmd = cmd.cmd.unwrap_or(Cmd::Status(cmd.status));

    match cmd {
        Cmd::Classes(cmd) => classes::handle(ctx, cmd).await,
        Cmd::Status(cmd) => status::handle(ctx, cmd).await,
        Cmd::Teardown(cmd) => teardown::handle(ctx, cmd).await,
    }
}
