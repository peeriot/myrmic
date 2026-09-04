use crate::args::Ctx;

mod classes;
mod status;
mod teardown;

#[derive(clap::Parser)]
pub struct Cells {
    #[clap(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    #[clap(alias = "class")]
    Classes(classes::Classes),
    Status(status::Status),
    Teardown(teardown::Teardown),
}

pub async fn handle(ctx: Ctx, cmd: Cells) -> anyhow::Result<()> {
    let cmd = cmd.cmd.unwrap_or(Cmd::Status(status::Status::default()));

    match cmd {
        Cmd::Classes(cmd) => classes::handle(ctx, cmd).await,
        Cmd::Status(cmd) => status::handle(ctx, cmd).await,
        Cmd::Teardown(cmd) => teardown::handle(ctx, cmd).await,
    }
}
