use std::io::{Read as _, Write as _};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::pin::pin;
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
use crate::utils::{apply_swarm_locator, build_filter};

#[derive(clap::Parser)]
pub struct Start {
    #[clap(short, long)]
    pub detached: bool,

    /// Directory holding runtime PID files, or an existing PID file.
    ///
    /// An existing file is used as the PID file itself. Any other path is a
    /// directory holding `<name>.pid`, created if its parent exists.
    ///
    /// Defaults to `$XDG_RUNTIME_DIR/myrmic/` when that directory exists (the
    /// per-user runtime tmpfs under systemd), otherwise
    /// `<tempdir>/myrmic-<user>/`.
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

    /// Endpoint to listen on, e.g. `tcp/0.0.0.0:7447`. Repeatable; merged with
    /// `zenoh.listen.endpoints` from the configuration file.
    #[clap(long, value_name = "ENDPOINT")]
    pub listen: Vec<String>,

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
        listen,
        tmp,
        path,
    } = cmd;

    let name = name.as_deref().unwrap_or(super::DEFAULT_RUNTIME_NAME);

    super::validate_runtime_name(name)?;
    let pid_path = match pid_path {
        Some(path) => path,
        None => {
            let path = default_pid_dir(super::DEFAULT_PID_DIR);
            if let Some(xdg) = xdg_runtime_dir().filter(|dir| !dir.is_dir()) {
                crate::info!(
                    &ctx,
                    "XDG_RUNTIME_DIR {} is not a directory; pid files go to {}",
                    xdg.display(),
                    path.display()
                );
            }
            path
        }
    };

    crate::debug!(&ctx, "pid-path: {}", pid_path.display());

    let pid = Pid::from_args(&pid_path, name);
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

    let mut config = read_config(path)?;

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

    // Only the group: a runtime merges its endpoints into the file's instead of
    // switching scouting off.
    apply_swarm_locator(&mut config.zenoh, &[], ctx.multicast_group)?;
    apply_endpoints(&mut config.zenoh, &ctx.connect, &listen)?;

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
const STARTUP_TIMEOUT: Duration =
    swarm::PLUGIN_STARTUP_TIMEOUT.saturating_add(Duration::from_secs(10));

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
/// registered and leased, so cells can be placed on it), then keeps it up until a shutdown signal
/// or a plugin failure.
async fn serve(
    ctx: &Ctx,
    swarm: swarm::Swarm,
    name: &str,
    zid: ZenohId,
    status: Option<&StatusPipe>,
) -> anyhow::Result<()> {
    let detached = status.is_some();
    let mut shutdown = pin!(wait_for_shutdown(detached));

    // Biased so the signal handlers are installed before startup begins.
    let mut spawned = tokio::select! {
        biased;
        result = &mut shutdown => return result,
        spawned = async { swarm.spawn_in_place()?.wait().await } => spawned?,
    };

    match status {
        Some(status) => status.send(Ok(())),
        None => crate::info!(ctx, "runtime {name:?} ready ({zid})"),
    }

    let result = tokio::select! {
        result = &mut shutdown => result,
        err = spawned.plugin_failure() => {
            let err = err.context(format!("runtime {name:?} stopped"));
            // a detached runtime's stderr is /dev/null and its status was sent as ready
            tracing::error!("{}", crate::format_error(&err));
            Err(err)
        }
    };
    if result.is_err() {
        spawned.kill_async().await;
    }

    result
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

/// The runtime configuration in the file at `path`, or the default without one.
fn read_config(path: Option<PathBuf>) -> anyhow::Result<SwarmConfig> {
    let Some(path) = path else {
        return Ok(SwarmConfig::default());
    };
    let input = std::fs::read_to_string(&path)
        .with_context(|| format!("unable to read configuration file: {}", path.display()))?;
    serde_yaml::from_str::<SwarmConfig>(&input).map_err(|err| {
        anyhow::anyhow!(
            "unable to parse configuration file [{}]: {}",
            path.display(),
            err
        )
    })
}

/// Merges the `--connect` and `--listen` endpoints into the zenoh config the
/// way `--tag` merges tags: appended to the file's, a repeat is a no-op.
fn apply_endpoints(
    zenoh: &mut zenoh::Config,
    connect: &[String],
    listen: &[String],
) -> anyhow::Result<()> {
    merge_endpoints(zenoh, "connect/endpoints", "--connect", connect)?;
    merge_endpoints(zenoh, "listen/endpoints", "--listen", listen)
}

/// Appends `values` to the endpoint list at `key`. A file may give the list
/// per mode; the flags then go to the list of this runtime's mode.
fn merge_endpoints(
    zenoh: &mut zenoh::Config,
    key: &str,
    flag: &str,
    values: &[String],
) -> anyhow::Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    for value in values {
        zenoh::config::EndPoint::try_from(value.clone())
            .map_err(|err| anyhow::anyhow!("invalid {flag} endpoint {value:?}: {err}"))?;
    }

    let mode = zenoh
        .get_json("mode")
        .map_err(|err| anyhow::anyhow!("unable to read the zenoh mode: {err}"))?;
    let mode: Option<String> =
        serde_json::from_str(&mode).context("unable to parse the zenoh mode")?;
    let mode = mode.unwrap_or_else(|| "peer".to_owned());

    let current = zenoh
        .get_json(key)
        .map_err(|err| anyhow::anyhow!("unable to read zenoh {key}: {err}"))?;
    let mut current: serde_json::Value =
        serde_json::from_str(&current).with_context(|| format!("unable to parse zenoh {key}"))?;
    if current.is_null() {
        current = serde_json::Value::Array(vec![]);
    }
    let list = match &mut current {
        serde_json::Value::Array(list) => list,
        serde_json::Value::Object(modes) => modes
            .entry(mode)
            .or_insert_with(|| serde_json::Value::Array(vec![]))
            .as_array_mut()
            .with_context(|| format!("zenoh {key} holds no list for this runtime's mode"))?,
        other => anyhow::bail!("unexpected zenoh {key} in the configuration: {other}"),
    };
    for value in values {
        let value = serde_json::Value::String(value.clone());
        if !list.contains(&value) {
            list.push(value);
        }
    }

    zenoh
        .insert_json5(key, &current.to_string())
        .map_err(|err| anyhow::anyhow!("invalid {flag} endpoint: {err}"))
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
    let dir = super::identity_dir()?;
    let path = dir.join(format!("{name}.yaml"));
    std::fs::create_dir_all(&dir)?;
    record_identity(&dir, &path, &serde_yaml::to_string(&identity)?)
        .with_context(|| format!("failed to record identity {}", path.display()))?;
    crate::info!(ctx, "Starting runtime {name:?}({zid})",);
    Ok(zid)
}

/// Write `contents` to the new file `path` in `dir`, and have both on disk before returning. The
/// runtime's data lies under the id the file records, so a file that a power loss shortly after
/// the start leaves empty or loses would cut the runtime off its data.
fn record_identity(dir: &Path, path: &Path, contents: &str) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    // the file's name is the directory's data
    std::fs::File::open(dir)?.sync_all()
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

    fn endpoints(config: &zenoh::Config, key: &str) -> serde_json::Value {
        serde_json::from_str(&config.get_json(key).expect("known key")).expect("valid json")
    }

    #[test]
    fn a_connect_flag_fills_an_empty_config() {
        let mut config = zenoh::Config::default();
        apply_endpoints(&mut config, &strings(&["tcp/a:7447"]), &[]).expect("merges");
        assert_eq!(
            endpoints(&config, "connect/endpoints"),
            serde_json::json!(["tcp/a:7447"])
        );
    }

    #[test]
    fn a_connect_flag_is_appended_after_the_file_endpoints() {
        let mut config = zenoh::Config::default();
        config
            .insert_json5("connect/endpoints", r#"["tcp/b:7447"]"#)
            .expect("seed");
        apply_endpoints(&mut config, &strings(&["tcp/a:7447"]), &[]).expect("merges");
        assert_eq!(
            endpoints(&config, "connect/endpoints"),
            serde_json::json!(["tcp/b:7447", "tcp/a:7447"])
        );
    }

    #[test]
    fn a_connect_flag_repeating_the_file_is_a_no_op() {
        let mut config = zenoh::Config::default();
        config
            .insert_json5("connect/endpoints", r#"["tcp/a:7447"]"#)
            .expect("seed");
        apply_endpoints(&mut config, &strings(&["tcp/a:7447"]), &[]).expect("merges");
        assert_eq!(
            endpoints(&config, "connect/endpoints"),
            serde_json::json!(["tcp/a:7447"])
        );
    }

    #[test]
    fn a_mode_dependent_file_gets_the_flag_under_the_runtime_mode() {
        let mut config = zenoh::Config::default();
        config
            .insert_json5("connect/endpoints", r#"{ router: ["tcp/r:7447"] }"#)
            .expect("seed");
        apply_endpoints(&mut config, &strings(&["tcp/a:7447"]), &[]).expect("merges");
        assert_eq!(
            endpoints(&config, "connect/endpoints"),
            serde_json::json!({ "router": ["tcp/r:7447"], "peer": ["tcp/a:7447"] })
        );
    }

    #[test]
    fn a_listen_flag_joins_the_default_listeners_of_the_runtime_mode() {
        let mut config = zenoh::Config::default();
        let before = endpoints(&config, "listen/endpoints");
        apply_endpoints(&mut config, &[], &strings(&["tcp/0.0.0.0:7447"])).expect("merges");
        let after = endpoints(&config, "listen/endpoints");
        // zenoh's default listen list is per mode; the flag joins the peer list.
        let mut expected = before["peer"].as_array().expect("peer defaults").clone();
        expected.push(serde_json::json!("tcp/0.0.0.0:7447"));
        assert_eq!(after["peer"], serde_json::Value::Array(expected));
        assert_eq!(after["router"], before["router"]);
    }

    #[test]
    fn an_invalid_endpoint_names_its_flag() {
        let mut config = zenoh::Config::default();
        let err = apply_endpoints(&mut config, &strings(&["nonsense"]), &[]).expect_err("rejected");
        assert!(err.to_string().contains("--connect"), "{err}");
        let err = apply_endpoints(&mut config, &[], &strings(&["nonsense"])).expect_err("rejected");
        assert!(err.to_string().contains("--listen"), "{err}");
    }
}
