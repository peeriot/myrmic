//! `myrmic signal-layer driver`: an out-of-tree driver module.

use super::module::{self, Bus, ModuleArgs, ModuleKind};
use crate::args::Ctx;

#[derive(clap::Parser)]
pub struct Driver {
    #[clap(flatten)]
    module: ModuleArgs,

    /// The bus the driver talks over.
    #[clap(long)]
    pub(super) bus: Bus,
}

pub fn handle(ctx: &Ctx, cmd: Driver) -> anyhow::Result<()> {
    module::scaffold(ctx, ModuleKind::Driver(cmd.bus), cmd.module, None)
}
