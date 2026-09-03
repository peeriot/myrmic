use espflash::cli::MonitorArgs;

use crate::args::Ctx;
use crate::flash;

#[derive(clap::Parser)]
pub struct Monitor {
    #[clap(flatten)]
    args: MonitorArgs,
}

pub fn handle(ctx: Ctx, cmd: Monitor) -> anyhow::Result<()> {
    crate::log::adopt_log_crate(ctx);
    let config = flash::config()?;
    flash::picked(espflash::cli::serial_monitor(cmd.args, &config))
}
