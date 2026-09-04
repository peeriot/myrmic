use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context as _;

use crate::args::Ctx;

#[derive(clap::Parser)]
pub struct Logs {
    /// Keep the log open and print new lines as the runtime writes them.
    #[clap(short, long)]
    pub follow: bool,

    /// Directory holding runtime PID files, used to pick the runtime when
    /// no name is given. Defaults to the standard pid directory.
    #[clap(long = "pid-path")]
    pub pid_path: Option<PathBuf>,

    /// Name of the runtime (may also be given before the subcommand).
    ///
    /// Defaults to the only runtime with a pid file, else the only one running.
    pub name: Option<String>,
}

pub fn handle(ctx: Ctx, cmd: Logs) -> anyhow::Result<()> {
    let Logs {
        follow,
        pid_path,
        name,
    } = cmd;

    let (name, id) = super::resolve_runtime(name, pid_path.as_deref())?;
    let dir = super::runtime_data_dir(&id)?.join("logs");

    if !dir.is_dir() {
        anyhow::bail!(
            "no logs for runtime {name:?} at {} \
             (never started with file logging, or the config sets its own log directory)",
            dir.display()
        );
    }

    crate::debug!(&ctx, "log directory: {}", dir.display());

    let mut current = latest_log(&dir)?;
    let mut offset = 0;

    if let Some(path) = &current {
        offset = print_from(path, 0)?;
    } else if !follow {
        anyhow::bail!("no log files in {}", dir.display());
    }

    if !follow {
        return Ok(());
    }

    loop {
        std::thread::sleep(Duration::from_millis(250));

        if let Some(path) = &current {
            let len = std::fs::metadata(path).map_or(0, |meta| meta.len());
            if len < offset {
                // Truncated or replaced; start over.
                offset = 0;
            }
            if len > offset {
                offset = print_from(path, offset)?;
            }
        }

        let latest = latest_log(&dir)?;
        if latest != current {
            // The appender rolled over; drain the old file once more.
            if let Some(old) = &current {
                let _ = print_from(old, offset);
            }
            current = latest;
            offset = 0;
        }
    }
}

/// Prints `path` from `offset` to the end, returning the new offset.
fn print_from(path: &Path, offset: u64) -> anyhow::Result<u64> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("failed to open log file {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))?;

    let mut stdout = std::io::stdout().lock();
    let copied = std::io::copy(&mut file, &mut stdout)?;

    Ok(offset + copied)
}

/// The most recently written `*.log` file in `dir`, if any.
fn latest_log(dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read log directory {}", dir.display()))?;

    let files = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((entry.path(), modified))
        });

    Ok(pick_latest(files))
}

/// Newest by modification time, file name as the tie-breaker (rolled file
/// names are date-stamped, so they sort chronologically).
fn pick_latest(files: impl Iterator<Item = (PathBuf, SystemTime)>) -> Option<PathBuf> {
    files
        .max_by(|(a_path, a_time), (b_path, b_time)| {
            a_time.cmp(b_time).then_with(|| a_path.cmp(b_path))
        })
        .map(|(path, _)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn newest_file_wins() {
        let files = vec![
            (PathBuf::from("runtime.2026-08-26.log"), at(100)),
            (PathBuf::from("runtime.2026-08-27.log"), at(200)),
        ];
        assert_eq!(
            pick_latest(files.into_iter()),
            Some(PathBuf::from("runtime.2026-08-27.log"))
        );
    }

    #[test]
    fn equal_times_fall_back_to_the_name() {
        let files = vec![
            (PathBuf::from("runtime.2026-08-27.log"), at(100)),
            (PathBuf::from("runtime.2026-08-26.log"), at(100)),
        ];
        assert_eq!(
            pick_latest(files.into_iter()),
            Some(PathBuf::from("runtime.2026-08-27.log"))
        );
    }

    #[test]
    fn no_files_no_pick() {
        assert_eq!(pick_latest(std::iter::empty()), None);
    }
}
