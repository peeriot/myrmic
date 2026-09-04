use crate::args::Ctx;

mod status;

#[derive(clap::Parser)]
pub struct Network {
    #[clap(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    #[clap(alias = "info")]
    Status(status::Status),
}

pub async fn handle(ctx: Ctx, cmd: Network) -> anyhow::Result<()> {
    let cmd = cmd.cmd.unwrap_or(Cmd::Status(status::Status::default()));

    match cmd {
        Cmd::Status(cmd) => status::handle(ctx, cmd).await,
    }
}
