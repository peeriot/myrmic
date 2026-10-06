use std::io::{Read as _, Write as _};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use cell_protocol::CapabilityTag;
use cell_protocol::replication::{check_user_tag, runtime_tag};
use swarm::SwarmConfig;
use tokio::signal::ctrl_c;
use tokio::signal::unix::{SignalKind, signal};
use zenoh::config::ZenohId;

use crate::args::Ctx;
use crate::pid::*;
use crate::utils::build_filter;

#[derive(clap::Parser)]
pub struct Start {
    #[clap(short, long)]
    pub detached: bool,

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

    /// Name of this runtime instance.
    /// Must be unique among runtimes sharing the same pid directory.
    #[clap(long = "name", short = 'n')]
    pub name: Option<String>,

    /// Capability tag to advertise on this runtime. Repeatable.
    ///
    /// Tags are merged with any tags defined in the configuration file.
    #[clap(long = "tag", short = 't')]
    pub tags: Vec<String>,

    /// Use an ephemeral in-memory database, discarded when the runtime stops.
    ///
    /// Overrides any configured database directory. Without this, a runtime
    /// gets a persistent database under the data folder keyed by its stable id.
    #[clap(long)]
    pub tmp: bool,

    pub path: Option<PathBuf>,
}

pub fn handle(ctx: Ctx, cmd: Start) -> anyhow::Result<()> {
    let Start {
        detached,
        pid_path,
        name,
        tags,
        tmp,
        path,
    } = cmd;

    let name = name.as_deref().unwrap_or(super::DEFAULT_RUNTIME_NAME);

    super::validate_runtime_name(name)?;
    let pid_path = pid_path.unwrap_or_else(|| default_pid_dir(super::DEFAULT_PID_DIR));

    crate::debug!(&ctx, "pid-path: {}", pid_path.display());

    let pid = Pid::from_args(&pid_path, name)?;
    pid.ensure_parent()?;

    // Don't clobber a running runtime with the same name.
    // Don't care if it's stale, as we'll overwrite it.
    if let PidStatus::Running(existing) = pid.status() {
        anyhow::bail!(
            "runtime {:?} already running (pid {existing}); see {}",
            pid.file_stem(),
            pid.path.display()
        );
    }

    crate::info!(
        &ctx,
        "runtime {:?} pid file: {}",
        pid.file_stem(),
        pid.path.display()
    );

    let mut config = if let Some(path) = path {
        let input = std::fs::read_to_string(&path)
            .with_context(|| format!("unable to read configuration file: {}", path.display()))?;

        serde_yaml::from_str::<swarm::SwarmConfig>(&input).map_err(|err| {
            anyhow::anyhow!(
                "unable to parse configuration file [{}]: {}",
                path.display(),
                err
            )
        })?
    } else {
        SwarmConfig::default()
    };

    // Set some myrmic defaults.
    config.plugins.orchestration = Some(config.plugins.orchestration.unwrap_or_default());

    // A named runtime keeps its zenoh id across restarts (recorded in the
    // platform data folder), so the swarm sees the same node come back
    // rather than a stranger with the old node's cells.
    let zid = stable_zid(&ctx, name)?;
    config
        .zenoh
        .set_id(Some(zid))
        .map_err(|err| anyhow::anyhow!("failed to set runtime id: {err:?}"))?;

    {
        let mut db = config.plugins.db.take().unwrap_or_default();
        let mut exec = config.plugins.execution.take().unwrap_or_default();

        exec.set_name(String::from(name));

        let merged = node_tags(
            exec.take_capability_tags(),
            std::mem::take(&mut db.tags),
            tags,
            zid,
        )?;

        db.tags.clone_from(&merged);
        exec.set_capability_tags(merged.into_iter().map(CapabilityTag::new).collect());

        db.store.directory = resolve_db_directory(tmp, db.store.directory.take(), zid)?;

        config.plugins.db = Some(db);
        config.plugins.execution = Some(exec);
    }

    if config.telemetry.logs.env_filter.is_none() {
        config.telemetry.logs.env_filter = build_filter(&ctx);
    }

    // Logs roll into the runtime's data folder unless the config says otherwise.
    // Created up front so a bad directory fails here, not silently post-daemonize.
    if config.telemetry.logs.directory.is_none() {
        let dir = super::runtime_data_dir(&zid.to_string())?.join(super::LOGS_DIR);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create log directory {}", dir.display()))?;
        config.telemetry.logs.directory = Some(dir);
    }

    // main() defaults SIGPIPE so piped CLI output dies quietly; a long-lived
    // runtime must not be killable that way, so restore Rust's usual ignore.
    // SAFETY: installing a standard signal disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    let log_dir = config
        .telemetry
        .logs
        .directory
        .clone()
        .expect("the log directory is set above");
    let swarm = swarm::Swarm::new(config);

    if detached {
        start_detached(ctx, &pid, swarm, name, zid, &log_dir)
    } else {
        run(ctx, &pid, swarm, name, zid, None)
    }
}

/// How long a detached start waits for the daemon's status before killing it. Longer than the
/// swarm's own plugin startup timeout, so a daemon stuck there reports that itself; the margin
/// covers starting zenoh, which that timeout doesn't.
const STARTUP_TIMEOUT: Duration = swarm::PLUGIN_STARTUP_TIMEOUT
    .checked_add(Duration::from_secs(10))
    .expect("the startup timeout fits a Duration");

/// The first byte of the status on the readiness pipe; a failure's message follows it.
const STATUS_READY: u8 = 0;
const STATUS_FAILED: u8 = 1;

/// Starts the runtime as a daemon and returns once it is ready. Fails if the daemon reports a
/// failure or exits without a status, and kills it if it isn't ready within [`STARTUP_TIMEOUT`].
///
/// The daemon's stdio goes to /dev/null, so it reports its status through a pipe this process
/// waits on.
fn start_detached(
    ctx: Ctx,
    pid: &Pid,
    swarm: swarm::Swarm,
    name: &str,
    zid: ZenohId,
    log_dir: &Path,
) -> anyhow::Result<()> {
    let (reader, writer) = std::io::pipe().context("unable to create the readiness pipe")?;

    match fork::fork().context("unable to fork the runtime")? {
        fork::Fork::Parent(child) => {
            drop(writer);
            let status = match read_to_end_timeout(reader, STARTUP_TIMEOUT) {
                Ok(status) => status,
                Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {
                    // The daemon's own pid is unknown here, but it is in the process group
                    // `setsid` created, whose id is the forked child's pid.
                    // SAFETY: sending a signal has no memory-safety preconditions.
                    if unsafe { libc::kill(-child, libc::SIGKILL) } != 0 {
                        return Err(std::io::Error::last_os_error()).with_context(|| {
                            format!("unable to kill runtime {name:?}, which didn't become ready")
                        });
                    }
                    anyhow::bail!(
                        "runtime {name:?} wasn't ready after {STARTUP_TIMEOUT:?}; killed it"
                    );
                }
                Err(err) => return Err(err).context("unable to read the runtime's status"),
            };

            match status.split_first() {
                Some((&STATUS_READY, [])) => {
                    crate::info!(ctx, "runtime {name:?} ready ({zid})");
                    Ok(())
                }
                Some((&STATUS_FAILED, message)) => anyhow::bail!(
                    "runtime {name:?} failed to start: {}",
                    std::str::from_utf8(message).expect("the daemon sends a UTF-8 message")
                ),
                // killed, say: neither the error path nor the panic hook got to run
                None => anyhow::bail!(
                    "runtime {name:?} exited without reporting a status; see its logs in {}",
                    log_dir.display()
                ),
                Some(_) => panic!("invalid status from runtime {name:?}: {status:?}"),
            }
        }
        fork::Fork::Child => {
            drop(reader);
            let status = StatusPipe::install_hook(writer);
            let result =
                become_daemon().and_then(|()| run(ctx, pid, swarm, name, zid, Some(&status)));
            if let Err(err) = result {
                // Not returned to main, whose error print would only reach /dev/null; the status
                // carries it unless the runtime was already ready.
                status.send(Err(crate::format_error(&err)));
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

/// Reads `reader` to its end, or fails with [`std::io::ErrorKind::TimedOut`] if that takes longer
/// than `timeout`.
///
/// A pipe read can't time out, so the read runs on its own thread; after a timeout, that thread
/// ends with the pipe.
fn read_to_end_timeout(
    mut reader: std::io::PipeReader,
    timeout: Duration,
) -> std::io::Result<Vec<u8>> {
    let (read_tx, read_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut content = vec![];
        let read = reader.read_to_end(&mut content).map(|_| content);
        // after a timeout nobody receives anymore, and nothing is left to report to
        drop(read_tx.send(read));
    });

    match read_rx.recv_timeout(timeout) {
        Ok(read) => read,
        Err(RecvTimeoutError::Timeout) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("nothing to read within {timeout:?}"),
        )),
        Err(RecvTimeoutError::Disconnected) => {
            unreachable!("the reader thread sends before it ends")
        }
    }
}

/// What `std::panic::take_hook` returns.
type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// The daemon's end of the readiness pipe, which carries one status to the starting process.
///
/// Until that status is sent, a panic hook sends the panic as a failure, since the daemon's stderr
/// is /dev/null, and ends the daemon: a runtime that panicked while starting is not one to keep.
struct StatusPipe {
    writer: Arc<Mutex<Option<std::io::PipeWriter>>>,
    previous_hook: Mutex<Option<PanicHook>>,
}

impl StatusPipe {
    fn install_hook(writer: std::io::PipeWriter) -> Self {
        let writer = Arc::new(Mutex::new(Some(writer)));
        let previous_hook = std::panic::take_hook();
        let hook_writer = Arc::clone(&writer);
        std::panic::set_hook(Box::new(move |info| {
            send_status(&hook_writer, Err(info.to_string()));
            // the exit code of an unhandled panic
            std::process::exit(101);
        }));
        Self {
            writer,
            previous_hook: Mutex::new(Some(previous_hook)),
        }
    }

    /// Sends `status` unless a status was already sent, and reverts the panic hook.
    fn send(&self, status: Result<(), String>) {
        send_status(&self.writer, status);
        let previous_hook = self
            .previous_hook
            .lock()
            .expect("the hook lock is never held across a panic")
            .take();
        // None: the readiness report already reverted it, and this is a later failure
        if let Some(previous_hook) = previous_hook {
            std::panic::set_hook(previous_hook);
        }
    }
}

/// Writes `status` into the pipe and closes it, unless an earlier status already did (the readiness
/// report before a later failure, say).
fn send_status(writer: &Mutex<Option<std::io::PipeWriter>>, status: Result<(), String>) {
    let writer = writer
        .lock()
        .expect("the status lock is never held across a panic")
        .take();
    if let Some(mut writer) = writer {
        let bytes = match status {
            Ok(()) => vec![STATUS_READY],
            Err(message) => [&[STATUS_FAILED], message.as_bytes()].concat(),
        };
        if let Err(err) = writer.write_all(&bytes) {
            // the starting process is gone (killed, say). Once the runtime is ready, this goes to
            // its log file.
            tracing::warn!("unable to report the status to the starting process: {err}");
        }
    }
}

/// Turns the forked child into the daemon; returns only in the daemon.
///
/// `setsid` moves the child into a new session without a controlling terminal, so closing the
/// terminal (SIGHUP) or Ctrl+C there no longer reaches the runtime. That makes the child a session
/// leader, which would acquire a controlling terminal again by opening a terminal device (a serial
/// port, say); the second fork's child is not a session leader, so it can't.
fn become_daemon() -> anyhow::Result<()> {
    fork::setsid().context("unable to start a new session")?;
    fork::chdir().context("unable to change to the root directory")?;
    fork::redirect_stdio().context("unable to redirect stdio to /dev/null")?;

    match fork::fork().context("unable to fork the daemon")? {
        // stdio already goes to /dev/null, so exiting flushes nothing anywhere
        fork::Fork::Parent(_) => std::process::exit(0),
        fork::Fork::Child => Ok(()),
    }
}

/// Runs the runtime until a shutdown signal. `status` is the daemon's status pipe of a detached
/// start.
fn run(
    ctx: Ctx,
    pid: &Pid,
    swarm: swarm::Swarm,
    name: &str,
    zid: ZenohId,
    status: Option<&StatusPipe>,
) -> anyhow::Result<()> {
    // Any fork precedes the runtime, so a full worker pool is fork-safe.
    let workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    crate::block_on_with(workers, async move {
        pid.write_self().await?;

        let result = serve(&ctx, swarm, name, zid, status).await;

        if let Err(err) = pid.remove_async().await {
            crate::warn!(
                ctx,
                "failed to remove pid file {}: {err}",
                pid.path.display()
            );
        }

        result
    })
}

/// Starts the swarm, reports it ready once every plugin is (the exec plugin only after it
/// registered and leased, so cells can be placed on it), then waits for a shutdown signal.
async fn serve(
    ctx: &Ctx,
    swarm: swarm::Swarm,
    name: &str,
    zid: ZenohId,
    status: Option<&StatusPipe>,
) -> anyhow::Result<()> {
    let detached = status.is_some();
    let _guard = swarm.spawn_in_place()?.wait().await?;

    match status {
        Some(status) => status.send(Ok(())),
        None => crate::info!(ctx, "runtime {name:?} ready ({zid})"),
    }

    wait_for_shutdown(detached).await
}

/// Waits for a shutdown signal.
///
/// Detached daemons have no controlling terminal, so only SIGTERM is meaningful.
/// In-process runs additionally listen for Ctrl+C (SIGINT).
async fn wait_for_shutdown(detached: bool) -> anyhow::Result<()> {
    let mut term = signal(SignalKind::terminate())?;
    if detached {
        term.recv().await;
    } else {
        tokio::select! {
            res = ctrl_c() => res?,
            _ = term.recv() => {}
        }
    }
    Ok(())
}

/// The node's one tag set, handed to both the db and exec plugins: the
/// configured exec and db tags plus the `--tag` values, merged, deduped, and
/// stamped with the system tag naming this runtime. User-written tags must
/// not use the system prefix — a `@` tag is only ever stamped here.
fn node_tags(
    exec: Vec<CapabilityTag>,
    db: Vec<String>,
    flags: Vec<String>,
    zid: ZenohId,
) -> anyhow::Result<Vec<String>> {
    let mut merged: Vec<String> = Vec::new();

    for tag in exec
        .into_iter()
        .map(CapabilityTag::into_inner)
        .chain(db)
        .chain(flags)
    {
        check_tag(&tag)?;
        if !merged.contains(&tag) {
            merged.push(tag);
        }
    }

    merged.push(runtime_tag(zid.into()));
    Ok(merged)
}

fn check_tag(tag: &str) -> anyhow::Result<()> {
    check_user_tag(tag).map_err(|err| anyhow::anyhow!("invalid tag '{tag}': {err}"))
}

/// The stable zenoh id for a named runtime: reuses the recorded one, else
/// mints a fresh id and records it for every later start under this name.
fn stable_zid(ctx: &Ctx, name: &str) -> anyhow::Result<ZenohId> {
    if let Some(id) = super::load_runtime_id(name)? {
        let zid = ZenohId::from_str(&id)
            .map_err(|err| anyhow::anyhow!("invalid recorded id for runtime {name:?}: {err}"))?;
        crate::info!(ctx, "Starting runtime {name:?}({zid})",);
        return Ok(zid);
    }

    // Rejection is possible but vanishingly rare (ids must not end
    // in a zero byte), so mint-and-retry beats byte surgery.
    let zid = std::iter::repeat_with(|| {
        ZenohId::from_str(&uuid::Uuid::new_v4().simple().to_string()).ok()
    })
    .take(16)
    .flatten()
    .next()
    .context("failed to mint a new runtime id")?;

    let identity = super::RuntimeIdentity {
        id: zid.to_string(),
    };
    let path = super::identity_dir()?.join(format!("{name}.yaml"));
    std::fs::create_dir_all(path.parent().expect("identity path has a parent"))?;
    std::fs::write(&path, serde_yaml::to_string(&identity)?)
        .with_context(|| format!("failed to record identity {}", path.display()))?;
    crate::info!(ctx, "Starting runtime {name:?}({zid})",);
    Ok(zid)
}

/// The per-runtime db directory when the user configured none: keyed by the
/// stable id so the same node reclaims its data across restarts, with room for
/// other per-runtime state alongside it.
fn default_db_dir(base: &Path, zid: ZenohId) -> PathBuf {
    base.join(zid.to_string()).join(super::DEFAULT_DB_DIR)
}

/// Where the db stores its data. `--tmp` forces in-memory (`None`), overriding
/// any configured directory; otherwise a configured directory is kept, and a
/// runtime with neither gets the per-node default under the data folder.
fn resolve_db_directory(
    tmp: bool,
    configured: Option<PathBuf>,
    zid: ZenohId,
) -> anyhow::Result<Option<PathBuf>> {
    if tmp {
        return Ok(None);
    }
    match configured {
        Some(dir) => Ok(Some(dir)),
        None => Ok(Some(default_db_dir(&super::data_dir()?, zid))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zid() -> ZenohId {
        ZenohId::from_str("a0b1c2").expect("zenoh id")
    }

    #[test]
    fn tmp_forces_in_memory_over_a_configured_directory() {
        let dir =
            resolve_db_directory(true, Some(PathBuf::from("/data/mine")), zid()).expect("resolve");
        assert_eq!(dir, None);
    }

    #[test]
    fn a_configured_directory_is_kept() {
        let configured = PathBuf::from("/data/mine");
        let dir = resolve_db_directory(false, Some(configured.clone()), zid()).expect("resolve");
        assert_eq!(dir, Some(configured));
    }

    #[test]
    fn the_default_directory_is_keyed_by_stable_id() {
        let zid = zid();
        let dir = default_db_dir(&PathBuf::from("/base/myrmic"), zid);
        assert_eq!(dir, PathBuf::from(format!("/base/myrmic/{zid}/db")));
    }

    fn configured(tags: &[&str]) -> Vec<CapabilityTag> {
        tags.iter().map(|tag| CapabilityTag::new(*tag)).collect()
    }

    fn strings(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|tag| String::from(*tag)).collect()
    }

    #[test]
    fn all_sources_merge_deduped_and_the_runtime_tag_is_stamped() {
        let merged = node_tags(
            configured(&["linux", "region-1"]),
            strings(&["store", "region-1"]),
            strings(&["edge"]),
            zid(),
        )
        .expect("plain tags pass");

        assert_eq!(
            merged,
            strings(&["linux", "region-1", "store", "edge", "@a0b1c2"])
        );
    }

    #[test]
    fn a_flag_tag_with_the_system_prefix_is_refused() {
        let err = node_tags(configured(&[]), vec![], strings(&["@mine"]), zid())
            .expect_err("system prefix is refused");

        assert!(err.to_string().contains("invalid tag '@mine'"), "{err}");
    }

    #[test]
    fn a_configured_tag_with_the_system_prefix_is_refused() {
        let err = node_tags(configured(&["@a0b1c2"]), vec![], vec![], zid())
            .expect_err("exec tags are checked");
        assert!(err.to_string().contains("invalid tag '@a0b1c2'"), "{err}");

        let err = node_tags(configured(&[]), strings(&["@a0b1c2"]), vec![], zid())
            .expect_err("db tags are checked");
        assert!(err.to_string().contains("invalid tag '@a0b1c2'"), "{err}");
    }
}
