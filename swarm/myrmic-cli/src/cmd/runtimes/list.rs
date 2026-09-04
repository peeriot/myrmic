use std::path::PathBuf;

use crate::args::Ctx;
use crate::pid::*;

#[derive(clap::Parser)]
pub struct List {
    /// Path to enumerate. If the path is a directory, all `*.pid` files
    /// inside are listed; if it's a file, only that single PID is shown.
    /// Defaults to the standard pid directory.
    #[clap(long = "pid-path")]
    pub pid_path: Option<PathBuf>,

    /// Only show the runtime with this name.
    pub name: Option<String>,
}

pub fn handle(ctx: Ctx, cmd: List) -> anyhow::Result<()> {
    let List { pid_path, name } = cmd;

    let custom_path = pid_path.is_some();

    let path = pid_path.unwrap_or_else(|| default_pid_dir(super::DEFAULT_PID_DIR));

    crate::debug!(&ctx, "pid-path: {}", path.display());

    let Some(mut pids) = super::discover_pids(&path)? else {
        crate::info!(&ctx, "no local runtimes");
        return Ok(());
    };

    if let Some(name) = &name {
        pids.retain(|pid| pid.file_stem() == name.as_str());
        if pids.is_empty() {
            anyhow::bail!("no runtime {name:?} found in {}", path.display());
        }
    }

    if pids.is_empty() {
        if custom_path {
            crate::info!(&ctx, "no local runtimes in {}", path.display());
        } else {
            crate::info!(&ctx, "no local runtimes");
        }
        return Ok(());
    }

    if name.is_none() {
        crate::info!(&ctx, "{} local runtime(s)", pids.len());
    }
    for pid in &pids {
        print_pid_row(pid);
    }

    Ok(())
}

fn print_pid_row(pid: &Pid) {
    let status = match pid.status() {
        PidStatus::Running(p) => format!("running\tpid={p}"),
        PidStatus::Stale(p) => format!("stale\tpid={p}"),
        PidStatus::Absent => "invalid\tpid=?".to_owned(),
    };
    println!("{}\t{status}", pid.file_stem());
}
