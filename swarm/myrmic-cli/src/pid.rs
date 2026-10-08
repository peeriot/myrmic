use anyhow::Context as _;
use std::fs::File;
use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// A resolved PID file location + runtime name, plus the I/O operations that
/// go with it (status, lock, signal, remove).
///
/// A running runtime holds a POSIX write lock on its pid file for its whole
/// life. The kernel releases it the moment the process dies, however it dies,
/// and `F_GETLK` reports the holder's pid translated into the caller's pid
/// namespace. So liveness and the pid to signal both come from the lock; the
/// number written in the file is for humans only.
pub struct Pid {
    pub path: PathBuf,
}

/// State of a PID file as seen by `list` / status checks.
#[derive(Debug, PartialEq, Eq)]
pub enum PidStatus {
    /// A runtime holds the lock; its pid as seen from this process.
    Running(libc::pid_t),
    /// A runtime holds the lock from a pid namespace this process can't see
    /// into, so there is no pid to signal from here.
    Unreachable,
    /// The file exists but nothing holds its lock: the runtime is gone.
    Stale,
    /// No file at the expected path (or it can't be opened).
    Absent,
}

/// Result of sending a signal via [`Pid::send_signal`].
#[derive(Debug, PartialEq, Eq)]
pub enum SignalOutcome {
    /// No PID file at the expected path.
    NotFound,
    /// PID file existed but nothing holds its lock.
    Stale,
    /// The runtime is alive in a pid namespace we can't signal into.
    Unreachable,
    /// Signal was delivered.
    Sent(libc::pid_t),
}

/// The runtime's hold on its pid file. Dropping it releases the lock.
///
/// A POSIX lock is released when the process closes *any* descriptor of the
/// file, so nothing else in the runtime may open the pid file while this is
/// held.
#[derive(Debug)]
pub struct PidLock {
    _file: File,
    path: PathBuf,
}

impl PidLock {
    /// Removes the file, then lets the lock go with the descriptor. In that
    /// order, so nobody observes an unlocked file at the path.
    pub fn release(self) -> std::io::Result<()> {
        std::fs::remove_file(&self.path)
    }
}

fn whole_file_write_lock() -> libc::flock {
    // SAFETY: all-zero is a valid `flock`; the fields that matter are set below.
    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::c_short::try_from(libc::F_WRLCK).expect("F_WRLCK fits l_type");
    lock.l_whence = libc::c_short::try_from(libc::SEEK_SET).expect("SEEK_SET fits l_whence");
    lock
}

/// Who holds the write lock on an open pid file: never `Absent`.
fn lock_status(file: &File) -> std::io::Result<PidStatus> {
    let mut lock = whole_file_write_lock();
    // SAFETY: F_GETLK only reads and fills in `lock`.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &raw mut lock) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(if libc::c_int::from(lock.l_type) == libc::F_UNLCK {
        PidStatus::Stale
    } else if lock.l_pid > 0 {
        PidStatus::Running(lock.l_pid)
    } else {
        PidStatus::Unreachable
    })
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

    pub fn status(&self) -> PidStatus {
        let Ok(file) = File::open(&self.path) else {
            return PidStatus::Absent;
        };
        lock_status(&file).unwrap_or(PidStatus::Absent)
    }

    /// Takes the runtime lock on this file and records our pid in it, for
    /// the rest of the process's life. Fails if another runtime holds it.
    pub fn acquire(&self) -> anyhow::Result<PidLock> {
        // A runtime shutting down unlinks its file while still holding the
        // lock, so the inode we open may be gone from the path by the time we
        // lock it. Check after locking and start over if so.
        loop {
            let file = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&self.path)
                .with_context(|| format!("failed to open pid file {}", self.path.display()))?;

            let lock = whole_file_write_lock();
            // SAFETY: F_SETLK reads `lock`; it never blocks.
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &raw const lock) } != 0 {
                let err = std::io::Error::last_os_error();
                if !matches!(err.raw_os_error(), Some(libc::EAGAIN | libc::EACCES)) {
                    return Err(anyhow::Error::new(err)
                        .context(format!("failed to lock pid file {}", self.path.display())));
                }
                let name = self.file_stem();
                match lock_status(&file)? {
                    PidStatus::Running(pid) => anyhow::bail!(
                        "runtime {name:?} already running (pid {pid}); see {}",
                        self.path.display()
                    ),
                    PidStatus::Unreachable => anyhow::bail!(
                        "runtime {name:?} already running in another pid namespace; see {}",
                        self.path.display()
                    ),
                    // The holder went away between the two calls.
                    PidStatus::Stale | PidStatus::Absent => continue,
                }
            }

            let locked = file.metadata()?.ino();
            match std::fs::metadata(&self.path) {
                Ok(current) if current.ino() == locked => {}
                _ => continue,
            }

            file.set_len(0)?;
            (&file).write_all(format!("{}\n", std::process::id()).as_bytes())?;
            file.sync_all()?;
            return Ok(PidLock {
                _file: file,
                path: self.path.clone(),
            });
        }
    }

    /// Removes the file unless a runtime holds its lock. Taking the lock
    /// first means a runtime that started since the caller's liveness check
    /// keeps its file instead of losing it to the unlink.
    pub fn remove(&self) -> anyhow::Result<()> {
        self.acquire()?.release()?;
        Ok(())
    }

    pub fn sigterm(&self) -> anyhow::Result<SignalOutcome> {
        self.send_signal(libc::SIGTERM)
    }

    /// Sends `sig` to whichever process holds the lock on this file.
    pub fn send_signal(&self, sig: libc::c_int) -> anyhow::Result<SignalOutcome> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SignalOutcome::NotFound);
            }
            Err(err) => {
                return Err(anyhow::Error::new(err)
                    .context(format!("failed to open pid file {}", self.path.display())));
            }
        };
        // The holder's pid is always positive, so this can never become
        // `kill(0, ..)` (our process group) or `kill(-1, ..)` (everything).
        let pid = match lock_status(&file)
            .with_context(|| format!("failed to query lock on {}", self.path.display()))?
        {
            PidStatus::Running(pid) => pid,
            PidStatus::Unreachable => return Ok(SignalOutcome::Unreachable),
            PidStatus::Stale | PidStatus::Absent => return Ok(SignalOutcome::Stale),
        };

        // SAFETY: kill(2) has no memory safety concerns; we check the return value.
        let rc = unsafe { libc::kill(pid, sig) };
        if rc == 0 {
            return Ok(SignalOutcome::Sent(pid));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            // Died between the lock query and the signal.
            return Ok(SignalOutcome::Stale);
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

#[cfg(test)]
pub(crate) mod test_support {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    /// A child process holding the runtime lock on a pid file, standing in
    /// for a live runtime. The lock is released when it is dropped.
    pub struct LockHolder {
        pub pid: libc::pid_t,
        stop: libc::c_int,
    }

    /// Forks a child that takes the pid file lock and blocks until the holder
    /// is dropped. The child only makes async-signal-safe calls.
    pub fn hold_lock(path: &Path) -> LockHolder {
        let path = CString::new(path.as_os_str().as_bytes()).expect("path without NUL");
        let lock = super::whole_file_write_lock();
        let mut ready = [0; 2];
        let mut stop = [0; 2];
        // SAFETY: plain pipe(2) calls on valid arrays.
        unsafe {
            assert_eq!(libc::pipe(ready.as_mut_ptr()), 0);
            assert_eq!(libc::pipe(stop.as_mut_ptr()), 0);
        }
        // SAFETY: the child only calls open/fcntl/read/write/_exit.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork failed");
        if child == 0 {
            // SAFETY: see above.
            unsafe {
                libc::close(ready[0]);
                libc::close(stop[1]);
                let fd = libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o644);
                if fd < 0 || libc::fcntl(fd, libc::F_SETLK, &raw const lock) != 0 {
                    libc::_exit(1);
                }
                libc::write(ready[1], [1u8].as_ptr().cast(), 1);
                let mut byte = 0u8;
                libc::read(stop[0], (&raw mut byte).cast(), 1);
                libc::_exit(0);
            }
        }
        // SAFETY: parent side of the pipes.
        unsafe {
            libc::close(ready[1]);
            libc::close(stop[0]);
            let mut byte = 0u8;
            let n = libc::read(ready[0], (&raw mut byte).cast(), 1);
            libc::close(ready[0]);
            assert_eq!(n, 1, "lock holder failed to take the lock");
        }
        LockHolder {
            pid: child,
            stop: stop[1],
        }
    }

    impl Drop for LockHolder {
        fn drop(&mut self) {
            // SAFETY: closing our end unblocks the child's read; then reap it.
            unsafe {
                libc::close(self.stop);
                let mut status = 0;
                libc::waitpid(self.pid, &raw mut status, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::hold_lock;
    use super::*;

    fn pid_in(dir: &tempfile::TempDir) -> Pid {
        Pid::from_path(dir.path().join("rt.pid"))
    }

    /// Asks a child process what `F_GETLK` reports for the file: the pid of
    /// the holder, or `None` when unlocked. A process never sees its own
    /// locks, so this is how a test checks a lock it holds itself.
    fn holder_seen_by_another_process(pid: &Pid) -> Option<libc::pid_t> {
        let status = std::process::Command::new(std::env::current_exe().expect("test exe"))
            .args([
                "--exact",
                "pid::tests::probe_lock_holder_for_parent",
                "--nocapture",
            ])
            .env("MYRMIC_PROBE_PID_FILE", &pid.path)
            .output()
            .expect("run probe");
        let out = String::from_utf8_lossy(&status.stdout);
        let line = out
            .lines()
            .find_map(|l| l.strip_prefix("PROBE:"))
            .expect("probe output");
        line.trim().parse().ok().filter(|p| *p > 0)
    }

    /// Not a real test: the probe process for `holder_seen_by_another_process`.
    #[test]
    fn probe_lock_holder_for_parent() {
        let Ok(path) = std::env::var("MYRMIC_PROBE_PID_FILE") else {
            return;
        };
        let report = match Pid::from_path(PathBuf::from(path)).status() {
            PidStatus::Running(p) => p.to_string(),
            _ => String::from("0"),
        };
        println!("PROBE:{report}");
    }

    #[test]
    fn status_is_absent_without_a_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(pid_in(&dir).status(), PidStatus::Absent);
    }

    #[test]
    fn status_is_stale_when_nothing_holds_the_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        std::fs::write(&pid.path, format!("{}\n", std::process::id())).expect("write");

        assert_eq!(pid.status(), PidStatus::Stale);
    }

    #[test]
    fn status_reports_the_lock_holder_not_the_file_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        std::fs::write(&pid.path, "6\n").expect("write");
        let holder = hold_lock(&pid.path);

        assert_eq!(pid.status(), PidStatus::Running(holder.pid));
    }

    #[test]
    fn status_is_stale_once_the_holder_exits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        let holder = hold_lock(&pid.path);
        drop(holder);

        assert_eq!(pid.status(), PidStatus::Stale);
    }

    #[test]
    fn acquire_holds_the_lock_and_records_our_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);

        let _lock = pid.acquire().expect("acquire");

        let own = libc::pid_t::try_from(std::process::id()).expect("pid fits");
        assert_eq!(holder_seen_by_another_process(&pid), Some(own));
        let contents = std::fs::read_to_string(&pid.path).expect("read");
        assert_eq!(contents.trim(), own.to_string());
    }

    #[test]
    fn acquire_refuses_while_another_process_holds_the_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        let holder = hold_lock(&pid.path);

        let err = pid.acquire().expect_err("held elsewhere");
        assert!(err.to_string().contains("already running"), "{err}");
        assert!(err.to_string().contains(&holder.pid.to_string()), "{err}");
    }

    #[test]
    fn acquire_takes_over_a_stale_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        std::fs::write(&pid.path, "garbage\n").expect("write");

        let _lock = pid.acquire().expect("stale files are reclaimed");

        let contents = std::fs::read_to_string(&pid.path).expect("read");
        assert_eq!(contents.trim(), std::process::id().to_string());
    }

    #[test]
    fn release_removes_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        let lock = pid.acquire().expect("acquire");

        lock.release().expect("release");

        assert_eq!(pid.status(), PidStatus::Absent);
    }

    #[test]
    fn remove_unlinks_a_stale_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        std::fs::write(&pid.path, "garbage\n").expect("write");

        pid.remove().expect("remove");

        assert_eq!(pid.status(), PidStatus::Absent);
    }

    /// `delete` saw the file unlocked, then a `start` took the lock before
    /// the unlink. The new runtime must keep its file.
    #[test]
    fn remove_keeps_a_file_a_runtime_took_over() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        std::fs::write(&pid.path, "garbage\n").expect("write");
        let holder = hold_lock(&pid.path);

        let err = pid.remove().expect_err("a running runtime keeps its file");

        assert!(err.to_string().contains("already running"), "{err}");
        assert_eq!(pid.status(), PidStatus::Running(holder.pid));
    }

    #[test]
    fn signals_go_to_the_lock_holder_not_the_recorded_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        std::fs::write(&pid.path, "1\n").expect("write");
        let holder = hold_lock(&pid.path);

        let outcome = pid.send_signal(libc::SIGTERM).expect("signal");

        assert_eq!(outcome, SignalOutcome::Sent(holder.pid));
        let mut status = 0;
        // SAFETY: reaping our own child.
        let reaped = unsafe { libc::waitpid(holder.pid, &raw mut status, 0) };
        assert_eq!(reaped, holder.pid);
        assert!(libc::WIFSIGNALED(status), "child died of the signal");
        assert_eq!(libc::WTERMSIG(status), libc::SIGTERM);
    }

    #[test]
    fn signalling_an_unheld_file_is_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid = pid_in(&dir);
        std::fs::write(&pid.path, "1\n").expect("write");

        assert_eq!(pid.sigterm().expect("signal"), SignalOutcome::Stale);
    }

    #[test]
    fn signalling_without_a_file_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert_eq!(
            pid_in(&dir).sigterm().expect("signal"),
            SignalOutcome::NotFound
        );
    }
}
