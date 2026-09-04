use std::path::PathBuf;

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

    /// Name of this runtime instance.
    /// Must be unique among runtimes sharing the same pid directory.
    #[clap(num_args = 1, value_delimiter = ' ')]
    pub name: Vec<String>,
}

pub fn handle(ctx: Ctx, cmd: Delete) -> anyhow::Result<()> {
    let Delete {
        pid_path,
        yes: _,
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
        match pid.sigterm()? {
            SignalOutcome::Sent(p) => {
                crate::info!(
                    &ctx,
                    "sent SIGTERM to runtime {:?} (pid {p})",
                    pid.file_stem()
                );
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
