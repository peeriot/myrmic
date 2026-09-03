use crate::args::Ctx;

mod status;

#[derive(clap::Parser)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Network {
    #[clap(subcommand)]
    cmd: Option<Cmd>,

    // `status` is the default, so its arguments are accepted here directly.
    #[clap(flatten)]
    status: status::Status,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    #[clap(alias = "info")]
    Status(status::Status),
}

pub async fn handle(ctx: Ctx, cmd: Network) -> anyhow::Result<()> {
    let cmd = cmd.cmd.unwrap_or(Cmd::Status(cmd.status));

    match cmd {
        Cmd::Status(cmd) => status::handle(ctx, cmd).await,
    }
}
