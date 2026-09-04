use anyhow::Context as _;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// A resolved PID file location + runtime name, plus the I/O operations that
/// go with it (read, write, signal, remove).
pub struct Pid {
    pub path: PathBuf,
}

/// State of a PID file as seen by `list` / status checks.
pub enum PidStatus {
    /// PID file points at a live process.
    Running(libc::pid_t),
    /// PID file exists but the process is gone.
    Stale(libc::pid_t),
    /// File missing, unreadable, or doesn't contain a valid positive pid.
    Absent,
}

/// Result of sending a signal via [`Pid::send_signal`].
pub enum SignalOutcome {
    /// No PID file at the expected path.
    NotFound,
    /// PID file existed but the process was already gone.
    Stale(libc::pid_t),
    /// Signal was delivered.
    Sent(libc::pid_t),
}

impl Pid {
    /// Build from CLI args. Validates the name and applies the
    /// directory-vs-file resolution rule:
    ///
    /// - If `--pid-path` is an existing directory, the file is
    ///   `<path>/<name>.pid`.
    /// - Else if the path's parent is an existing directory, the path itself
    ///   is taken as the PID file (user passed a full file path).
    /// - Else the path is treated as a directory to be created, with
    ///   `<name>.pid` inside.
    pub fn from_args(path: &Path, name: &str) -> anyhow::Result<Self> {
        fn is_directory(path: &Path) -> bool {
            path.is_dir()
                || path
                    .parent()
                    .is_some_and(|p| p.is_dir() || p.as_os_str().is_empty())
        }

        let path = if path.is_file() {
            path.to_path_buf()
        } else if is_directory(path) {
            path.join(format!("{name}.pid"))
        } else {
            anyhow::bail!("unable to determine pid path: {}", path.display());
        };

        Ok(Self { path })
    }

    /// Build from an existing PID file
    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn file_stem(&self) -> &str {
        self.path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
    }

    /// Ensure the directory containing the PID file exists.
    pub fn ensure_parent(&self) -> anyhow::Result<()> {
        let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) else {
            return Ok(());
        };
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create pid directory {}", parent.display()))
    }

    /// Read and parse the pid from the file.
    /// `Ok(None)` if the file contains garbage or a non-positive pid.
    pub fn read(&self) -> std::io::Result<Option<libc::pid_t>> {
        let contents = std::fs::read_to_string(&self.path)?;
        // Filtering for `> 0` guards against against:
        //  `kill(0, ...)` (which kills the whole process group)
        //  `kill(-1, ...)` (every process we can signal)
        // which a truncated or garbage pid file could otherwise produce.
        Ok(contents
            .trim()
            .parse::<libc::pid_t>()
            .ok()
            .filter(|pid| *pid > 0))
    }

    pub fn status(&self) -> PidStatus {
        match self.read() {
            Ok(Some(pid)) if process_is_alive(pid) => PidStatus::Running(pid),
            Ok(Some(pid)) => PidStatus::Stale(pid),
            _ => PidStatus::Absent,
        }
    }

    /// Write the current process's pid into this file. `fsync`s so the file
    /// is on disk by the time the runtime starts accepting work.
    pub async fn write_self(&self) -> anyhow::Result<()> {
        let pid = std::process::id();
        let mut file = tokio::fs::File::create(&self.path).await?;
        file.write_all(format!("{pid}\n").as_bytes()).await?;
        file.flush().await?;
        file.sync_all().await?;
        Ok(())
    }

    pub fn remove(&self) -> std::io::Result<()> {
        std::fs::remove_file(&self.path)
    }

    pub async fn remove_async(&self) -> std::io::Result<()> {
        tokio::fs::remove_file(&self.path).await
    }

    pub fn sigterm(&self) -> anyhow::Result<SignalOutcome> {
        self.send_signal(libc::SIGTERM)
    }

    /// Sends `sig` to the pid recorded in this file.
    pub fn send_signal(&self, sig: libc::c_int) -> anyhow::Result<SignalOutcome> {
        let pid = match self.read() {
            Ok(Some(pid)) => pid,
            Ok(None) => {
                anyhow::bail!("pid file {} contains invalid pid", self.path.display())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SignalOutcome::NotFound);
            }
            Err(err) => {
                return Err(anyhow::Error::new(err)
                    .context(format!("failed to read pid file {}", self.path.display())));
            }
        };

        // SAFETY: kill(2) has no memory safety concerns; we check the return value.
        let rc = unsafe { libc::kill(pid, sig) };
        if rc == 0 {
            return Ok(SignalOutcome::Sent(pid));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            // Process is already dead, but the file wasn't cleaned up...
            return Ok(SignalOutcome::Stale(pid));
        }
        Err(anyhow::anyhow!("failed to signal pid {pid}: {err}"))
    }
}

/// Picks a default directory for runtime PID files.
///
/// Most systems define a per-user tmpfs via systemd as a runtime directory.
/// Thankfully, that's generally defined by the `$XDG_*` family of env vars.
/// In this case, we want `$XDG_RUNTIME_DIR`.
///
/// If that's not set, then we just fall back to the tmp directory (as defined by `std::env::temp_dir`).
///
/// All said, this tries to isolate user pids, so there's no collisions.
pub fn default_pid_dir(pid_group: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join(pid_group);
    }

    let user_dir = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
        .filter(|v| !v.is_empty());

    let dir_name = match user_dir {
        Some(user) => format!("{}-{}", pid_group, user),
        None => pid_group.to_owned(),
    };

    std::env::temp_dir().join(dir_name)
}

/// Returns `true` if a process with `pid` exists.
fn process_is_alive(pid: libc::pid_t) -> bool {
    // `kill(pid, 0)` performs the permission check, doesn't actually kill anything... linux amirite
    // SAFETY: signal 0 delivers nothing; we only read the return value.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // `ESRCH` is the only "dead" answer, `EPERM` means we're not the owner.
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
