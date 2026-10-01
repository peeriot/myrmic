use std::fs::File;
use std::io::{IsTerminal as _, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context as _;

use crate::args::Ctx;

#[derive(clap::Parser)]
pub struct Logs {
    /// Keep the log open and print new lines as the runtime writes them.
    #[clap(short, long)]
    pub follow: bool,

    /// Number of trailing lines to print from the current log.
    ///
    /// Defaults to the whole file on a terminal, else the last 10 lines.
    #[clap(short = 'n', long)]
    pub lines: Option<usize>,

    /// Directory holding runtime PID files, used to pick the runtime when
    /// no name is given. Defaults to the standard pid directory.
    #[clap(long = "pid-path")]
    pub pid_path: Option<PathBuf>,

    /// Name of the runtime (may also be given before the subcommand).
    ///
    /// Defaults to the only runtime with a pid file, else the only one running.
    pub name: Option<String>,
}

pub fn handle(ctx: &Ctx, cmd: Logs) -> anyhow::Result<()> {
    let Logs {
        follow,
        lines,
        pid_path,
        name,
    } = cmd;

    let (name, id) = super::resolve_runtime(name, pid_path.as_deref())?;
    let dir = super::runtime_data_dir(&id)?.join(super::LOGS_DIR);

    if !dir.is_dir() {
        anyhow::bail!(
            "no logs for runtime {name:?} at {} \
             (never started with file logging, or the config sets its own log directory)",
            dir.display()
        );
    }

    crate::debug!(&ctx, "log directory: {}", dir.display());

    let mut current = latest_log(&dir)?;
    if current.is_none() && !follow {
        anyhow::bail!("no log files in {}", dir.display());
    }

    let mut stdout = std::io::stdout().lock();
    let lines = lines.or_else(|| (!stdout.is_terminal()).then_some(10));

    let mut log = current
        .as_deref()
        .map(|path| {
            let mut log = LogFile::open(path)?;
            let start = match lines {
                Some(lines) => tail_start(&mut log.file, lines)?,
                None => 0,
            };
            log.print_from(start, &mut stdout)?;
            anyhow::Ok(log)
        })
        .transpose()?;

    if !follow {
        return Ok(());
    }

    loop {
        std::thread::sleep(Duration::from_millis(250));

        if let Some(log) = &mut log
            && log.drain(&mut stdout)?
        {
            continue;
        }

        // A rolled-over file stops growing, so only rescan when idle.
        let latest = latest_log(&dir)?;
        if latest != current {
            if let Some(old) = &mut log {
                let _ = old.drain(&mut stdout);
            }
            log = latest
                .as_deref()
                .map(|path| {
                    let mut log = LogFile::open(path)?;
                    log.print_from(0, &mut stdout)?;
                    anyhow::Ok(log)
                })
                .transpose()?;
            current = latest;
        }
    }
}

/// An open log file and how far into it has been printed.
struct LogFile {
    file: File,
    pos: u64,
}

impl LogFile {
    fn open(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("failed to open log file {}", path.display()))?;
        Ok(Self { file, pos: 0 })
    }

    fn print_from(&mut self, offset: u64, out: &mut impl Write) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        let copied = std::io::copy(&mut self.file, out)?;
        self.pos = offset + copied;
        out.flush()
    }

    /// Prints whatever was appended since the last read; returns whether
    /// there was anything.
    fn drain(&mut self, out: &mut impl Write) -> std::io::Result<bool> {
        let len = self.file.metadata()?.len();
        if len == self.pos {
            return Ok(false);
        }
        // Shorter than what was printed means truncated; start over.
        let from = if len < self.pos { 0 } else { self.pos };
        self.print_from(from, out)?;
        Ok(true)
    }
}

/// Byte offset where the last `lines` lines of `file` begin, scanning
/// backwards so large logs aren't read in full.
fn tail_start(file: &mut (impl Read + Seek), lines: usize) -> std::io::Result<u64> {
    let len = file.seek(SeekFrom::End(0))?;
    if lines == 0 {
        return Ok(len);
    }

    let mut buf = [0u8; 8192];
    let mut remaining = lines;
    let mut end = len;
    // A final newline terminates the last line rather than starting a new one.
    let mut at_last_byte = true;

    while end > 0 {
        let chunk_len = usize::try_from(end).map_or(buf.len(), |end| end.min(buf.len()));
        let start = end - chunk_len as u64;
        let chunk = &mut buf[..chunk_len];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(chunk)?;

        for (i, &byte) in chunk.iter().enumerate().rev() {
            if std::mem::take(&mut at_last_byte) || byte != b'\n' {
                continue;
            }
            remaining -= 1;
            if remaining == 0 {
                return Ok(start + i as u64 + 1);
            }
        }
        end = start;
    }

    Ok(0)
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
    use std::io::Cursor;

    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn tail(content: &str, lines: usize) -> &str {
        let start = tail_start(&mut Cursor::new(content), lines).unwrap();
        &content[usize::try_from(start).unwrap()..]
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

    #[test]
    fn tail_takes_last_lines() {
        assert_eq!(tail("a\nb\nc\n", 2), "b\nc\n");
        assert_eq!(tail("a\nb\nc", 2), "b\nc");
    }

    #[test]
    fn tail_with_fewer_lines_prints_everything() {
        assert_eq!(tail("a\nb\n", 10), "a\nb\n");
        assert_eq!(tail("", 10), "");
    }

    #[test]
    fn tail_zero_lines_prints_nothing() {
        assert_eq!(tail("a\nb\n", 0), "");
    }

    #[test]
    fn tail_spans_chunks() {
        let long = "x".repeat(10_000);
        let content = format!("first\n{long}\nlast\n");
        assert_eq!(tail(&content, 2), format!("{long}\nlast\n"));
        assert_eq!(tail(&content, 3), content);
    }
}
