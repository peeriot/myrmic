use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context as _;

use crate::args::Ctx;
use crate::pid::*;

#[derive(clap::Parser)]
pub struct Info {
    /// Directory holding runtime PID files, used to pick the runtime when
    /// no name is given and to report whether it is running.
    /// Defaults to the standard pid directory.
    #[clap(long = "pid-path")]
    pub pid_path: Option<PathBuf>,

    /// Name of the runtime (may also be given before the subcommand).
    ///
    /// Defaults to the only runtime with a pid file, else the only one running.
    pub name: Option<String>,
}

pub fn handle(_ctx: Ctx, cmd: Info) -> anyhow::Result<()> {
    let Info { pid_path, name } = cmd;

    let (name, id) = super::resolve_runtime(name, pid_path.as_deref())?;

    let runtime_dir = super::runtime_data_dir(&id)?;
    let db_dir = runtime_dir.join("db");

    let pid_path = pid_path.unwrap_or_else(|| default_pid_dir(super::DEFAULT_PID_DIR));
    let status = match Pid::from_args(&pid_path, &name)?.status() {
        PidStatus::Running(pid) => format!("running (pid {pid})"),
        PidStatus::Stale(_) | PidStatus::Absent => String::from("stopped"),
    };

    println!("runtime   {name} ({id})");
    println!("status    {status}");
    println!("data      {}", runtime_dir.display());

    if !db_dir.is_dir() {
        println!("db        {} (missing)", db_dir.display());
        println!();
        println!(
            "no database on disk: the runtime never started, runs with --tmp (in-memory),\n\
             or stores its database in a directory set by its configuration file"
        );
        return Ok(());
    }

    let stats = dir_stats(&db_dir)?;

    #[allow(clippy::cast_precision_loss)]
    let size = human_bytes::human_bytes(stats.bytes as f64);

    println!("db        {}", db_dir.display());
    println!("size      {size} across {} file(s)", stats.files);

    if let Some(modified) = stats.newest {
        let ago = match modified.elapsed() {
            Ok(elapsed) => {
                let elapsed = std::time::Duration::from_secs(elapsed.as_secs());
                format!("{} ago", humantime::format_duration(elapsed))
            }
            Err(_) => String::from("in the future?"),
        };
        println!("written   {ago}");
    }

    Ok(())
}

#[derive(Default)]
struct DirStats {
    bytes: u64,
    files: u64,
    newest: Option<SystemTime>,
}

fn dir_stats(path: &Path) -> anyhow::Result<DirStats> {
    let mut stats = DirStats::default();
    let mut stack = vec![path.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?;

        for entry in entries.filter_map(Result::ok) {
            let Ok(meta) = entry.metadata() else {
                continue;
            };

            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                // Actual disk usage, not apparent size: the db's journals are
                // preallocated sparse files that would dwarf the real data.
                stats.bytes += std::os::unix::fs::MetadataExt::blocks(&meta) * 512;
                stats.files += 1;
                if let Ok(modified) = meta.modified() {
                    stats.newest = Some(stats.newest.map_or(modified, |n| n.max(modified)));
                }
            }
        }
    }

    Ok(stats)
}
