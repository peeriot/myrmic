use crate::args::Ctx;

mod add;
mod delete;
mod info;
mod list;

#[derive(clap::Parser)]
pub struct Classes {
    #[clap(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    List(list::List),
    Add(add::Add),
    #[clap(alias = "remove", alias = "rm")]
    Delete(delete::Delete),
    Info(info::Info),
}

pub async fn handle(ctx: Ctx, cmd: Classes) -> anyhow::Result<()> {
    let cmd = cmd.cmd.unwrap_or(Cmd::List(list::List::default()));

    match cmd {
        Cmd::List(cmd) => list::handle(ctx, cmd).await,
        Cmd::Add(cmd) => add::handle(ctx, cmd).await,
        Cmd::Delete(cmd) => delete::handle(ctx, cmd).await,
        Cmd::Info(cmd) => info::handle(ctx, cmd).await,
    }
}
