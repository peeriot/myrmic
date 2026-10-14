use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::args::Ctx;
use crate::pid::*;

#[derive(clap::Parser)]
pub struct Delete {
    /// Directory holding runtime PID files, or a full PID file path.
    ///
    /// If the given path is (or will be created as) a directory, the PID
    /// file sits inside it as `<name>.pid`. If the path's parent is an
    /// existing directory, the path itself is taken as the PID file.
    ///
    /// Defaults to `$XDG_RUNTIME_DIR/myrmic/` when set (the per-user
    /// runtime tmpfs under systemd), otherwise `<tempdir>/myrmic-<user>/`.
    #[clap(long = "pid-path")]
    pub pid_path: Option<PathBuf>,

    #[clap(short = 'y')]
    pub yes: bool,

    /// Signal sent to the runtime.
    ///
    /// `term` lets the runtime shut down: it says goodbye to the swarm and
    /// cleans up. `kill` ends it abruptly, as a crash or power loss would.
    #[clap(long, value_enum, default_value_t = Signal::Term)]
    pub signal: Signal,

    /// Name of this runtime instance.
    /// Must be unique among runtimes sharing the same pid directory.
    #[clap(num_args = 1, value_delimiter = ' ')]
    pub name: Vec<String>,
}

/// The signals [`Delete`] can send: both end the runtime, so the delete can
/// wait for it to exit.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Signal {
    /// SIGTERM
    Term,
    /// SIGKILL
    Kill,
}

impl Signal {
    fn number(self) -> libc::c_int {
        match self {
            Self::Term => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }
}

pub fn handle(ctx: &Ctx, cmd: Delete) -> anyhow::Result<()> {
    let Delete {
        pid_path,
        yes: _,
        signal,
        name: mut names,
    } = cmd;

    let pid_path = pid_path.unwrap_or_else(|| default_pid_dir(super::DEFAULT_PID_DIR));

    if names.is_empty() {
        names.push(super::implicit_runtime_name(&pid_path)?);
    }

    names
        .iter()
        .try_for_each(|name| super::validate_runtime_name(name))?;

    let Some(mut all_pids) = super::discover_pids(&pid_path)? else {
        anyhow::bail!("no runtimes at {}", pid_path.display());
    };

    let mut filtered = vec![];
    while let Some(name) = names.pop() {
        let pos = all_pids
            .iter()
            .position(|pid| pid.file_stem() == name.as_str());

        let Some(pos) = pos else {
            anyhow::bail!("no runtime {name:?} found in {}", pid_path.display());
        };

        let pid = all_pids.swap_remove(pos);
        filtered.push(pid);
    }
    let pids = filtered;

    for pid in pids {
        match pid.send_signal(signal.number())? {
            SignalOutcome::Sent(p) => {
                crate::info!(
                    &ctx,
                    "sent {} to runtime {:?} (pid {p})",
                    signal.name(),
                    pid.file_stem()
                );
                wait_for_exit(&pid, p)?;
            }
            SignalOutcome::Stale(p) => {
                crate::warn!(
                    ctx,
                    "no process with pid {p}; removing stale pid file {}",
                    pid.path.display()
                );
                if let Err(err) = pid.remove() {
                    crate::warn!(
                        ctx,
                        "failed to remove stale pid file {}: {err}",
                        pid.path.display()
                    );
                }
            }
            SignalOutcome::NotFound => {
                anyhow::bail!(
                    "no runtime {:?} (pid file {} not found)",
                    pid.file_stem(),
                    pid.path.display()
                );
            }
        }
    }

    Ok(())
}

/// Wait for a signalled runtime's process to exit, then remove its PID file.
///
/// Waits on the process, not on the file: the runtime removes its PID file as
/// soon as it receives the SIGTERM, before it has shut down and released its
/// resources (e.g. its listen port). The process exits without cleaning up the
/// file on a SIGKILL, and on a SIGTERM that arrives during daemon startup
/// before the runtime installs its shutdown handler.
fn wait_for_exit(pid: &Pid, process: libc::pid_t) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_is_alive(process) {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "runtime {:?} (pid {process}) did not stop within 10 seconds",
                pid.file_stem()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if let Err(err) = pid.remove()
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return Err(anyhow::Error::new(err));
    }
    Ok(())
}
