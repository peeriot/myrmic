use myrmic_build::PlatformFamily;

use crate::args::Ctx;

#[derive(clap::Parser)]
pub struct Platforms {}

#[allow(clippy::unnecessary_wraps)]
pub fn handle(_ctx: Ctx, _cmd: Platforms) -> anyhow::Result<()> {
    let width = PlatformFamily::ALL
        .iter()
        .map(|p| p.name().len())
        .max()
        .unwrap_or(0);

    for platform in PlatformFamily::ALL {
        let aliases = platform.aliases();
        if aliases.is_empty() {
            println!("{:width$}", platform.name(), width = width);
        } else {
            println!(
                "{:width$} (aliases: {})",
                platform.name(),
                aliases.join(", "),
                width = width,
            );
        }
    }

    Ok(())
}
