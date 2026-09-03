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
    #[clap(alias = "class")]
    Classes(classes::Classes),
    Status(status::Status),
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
