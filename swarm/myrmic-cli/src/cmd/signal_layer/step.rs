//! `myrmic signal-layer step`: an out-of-tree processing step module.

use super::module::{self, ModuleArgs, ModuleKind};
use crate::args::Ctx;

#[derive(clap::Parser)]
pub struct Step {
    #[clap(flatten)]
    module: ModuleArgs,

    /// Where the myrmic crates come from: a version, a git URL or a local
    /// checkout.
    #[clap(long, alias = "repo")]
    sdk: Option<String>,
}

pub fn handle(ctx: &Ctx, cmd: Step) -> anyhow::Result<()> {
    module::scaffold(ctx, ModuleKind::Step, cmd.module, cmd.sdk.as_deref())
}
