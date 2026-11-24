//! Shim around the myrmic CLI (runtimes, deploys, telemetry) running on the host, on a remote
//! host over SSH, or inside a docker container.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

pub use backend::MyrmicBackend;
pub use backend::{Docker, Local, Ssh};
use cell_protocol::Sri;
pub use debug::{DebugEntry, DebugListener, DebugLog};
use myrmic_build::PlatformFamily;
use sorg_common::RestartType;

use crate::{
    CommandOutput,
    clients::sorg::SorgHandle,
    docker::container::ManagedContainer,
    myrmic::cell::{CellSpec, DeployedApp, DeployedCell, TargetTempDir},
};

pub mod backend;
pub mod cell;
mod debug;
pub mod mqtt;

/// Prefix of the CLI's info lines on stderr (the label is padded to the width of `ERROR`).
const INFO_PREFIX: &str = "INFO  ";

/// What `myrmic runtimes delete` prints when no runtime of that name is running: there is no pid
/// file for it, or no pid directory at all.
pub(crate) const RUNTIME_GONE: [&str; 2] = ["no runtime \"", "no runtimes at "];

/// What `myrmic delete` prints when nothing by that name is deployed (any more).
pub(crate) const DEPLOYMENT_GONE: [&str; 2] = ["nothing named '", "no deployed cell '"];

/// whether a delete's `stderr` says there was nothing left to delete, i.e. contains one of `texts`
/// ([`RUNTIME_GONE`] or [`DEPLOYMENT_GONE`])
pub(crate) fn already_gone(stderr: &str, texts: &[&str]) -> bool {
    texts.iter().any(|text| stderr.contains(text))
}

/// Why a myrmic operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// the CLI exited unsuccessfully; `stderr` starts with the invocation and its exit code
    #[error("{stderr}")]
    Cli { stderr: String },
    /// the CLI succeeded, but the state it leads to did not show up in time
    #[error("timed out waiting for {waited_for}")]
    Timeout { waited_for: String },
}

/// A shim around the myrmic CLI. It knows the CLI's commands and runs them on any
/// [`MyrmicBackend`]: on the host, on a remote host over SSH, or inside a container.
///
/// Every operation returns a [`Result`]; a test `.unwrap()`s or `.expect(..)`s what it assumes to
/// succeed. A command that cannot be started at all (binary, `ssh` or `docker` missing, host or
/// daemon unreachable) is broken test infrastructure, not a result, and panics.
#[derive(Clone)]
pub struct Myrmic<B> {
    backend: B,
    /// the myrmic binary, as the backend's commands find it on the target
    binary: String,
}

impl Myrmic<Local> {
    /// create a shim around a locally built myrmic binary (see [`crate::resolve_binary!`])
    pub fn local() -> Self {
        Self::local_on(Local::default())
    }

    /// Like [`Self::local`], but nothing outside this shim and its clones sees its runtime, and
    /// it sees no other runtime, so tests using it can run in parallel.
    ///
    /// The shim gets its own state directories and its own multicast group, on which its
    /// runtimes scout and every CLI call (through `DEFAULT_MYRMIC_MULTICAST_GROUP`) and
    /// sessions from [`Local::zenoh_config`] discover them, the same way as on the default group.
    /// [`RuntimeBuilder::config`] is not available, the shim generates the config itself.
    pub fn local_isolated() -> Self {
        Self::local_on(Local::isolated())
    }

    /// Like [`Self::local`], but every CLI call (through `DEFAULT_MYRMIC_CONNECT`) and sessions
    /// from [`Local::zenoh_config`] reach the swarm through `endpoint` (e.g. `tcp/192.0.2.10:7447`,
    /// a router's) instead of by multicast scouting. The swarm runs elsewhere: the shim starts no
    /// runtime.
    pub fn local_connected(endpoint: impl Into<String>) -> Self {
        Self::local_on(Local::connected(endpoint.into()))
    }

    /// A shim on this [`Self::local_isolated`] shim's state directories but on a multicast group
    /// of its own. A runtime it starts has the identity and database it has on this shim, but
    /// reaches none of this shim's runtimes, nor those of another cut-off shim.
    pub fn cut_off(&self) -> Self {
        Self {
            backend: self.backend.cut_off(),
            binary: self.binary.clone(),
        }
    }

    /// the locally built myrmic binary on `backend`
    fn local_on(backend: Local) -> Self {
        let binary = crate::resolve_binary!("myrmic");
        Self {
            backend,
            binary: binary
                .to_str()
                .unwrap_or_else(|| panic!("myrmic binary path {} is not UTF-8", binary.display()))
                .to_owned(),
        }
    }
}

impl Myrmic<Docker> {
    /// create a shim that runs the myrmic CLI inside `container`, which has `myrmic` on its
    /// `PATH`; the container is removed once the last shim or guard using it drops
    pub fn attach(container: Arc<ManagedContainer>) -> Self {
        Self {
            backend: Docker::attach(container),
            binary: "myrmic".to_owned(),
        }
    }
}

impl Myrmic<Ssh> {
    /// create a shim that runs the myrmic CLI on `host` over SSH, resolving `myrmic` on the
    /// remote user's `PATH`
    pub fn ssh(host: impl Into<String>) -> Self {
        Self {
            backend: Ssh::new(host),
            binary: "myrmic".to_owned(),
        }
    }

    /// like [`Self::ssh`], but the remote myrmic binary lives at `binary` rather than on `PATH`
    /// (e.g. a path a benchmark harness `scp`'d it to)
    pub fn ssh_at(host: impl Into<String>, binary: impl Into<String>) -> Self {
        Self {
            backend: Ssh::new(host),
            binary: binary.into(),
        }
    }

    /// Like [`Self::ssh_at`], but isolated like [`Self::local_isolated`]: on state directories
    /// of its own on `host`, removed once the last clone of this shim is dropped, and on
    /// `multicast_group`, which the runtimes of one swarm share.
    pub(crate) fn ssh_isolated(
        host: impl Into<String>,
        binary: impl Into<String>,
        multicast_group: String,
    ) -> Self {
        Self {
            backend: Ssh::isolated(host, multicast_group),
            binary: binary.into(),
        }
    }
}

impl<B> Myrmic<B>
where
    B: MyrmicBackend,
{
    /// where this shim's commands run
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// describe a `myrmic runtimes start` with `tags`; finish with [`RuntimeBuilder::start`]
    pub fn runtime<'a>(&'a self, tags: &'a [&'a str]) -> RuntimeBuilder<'a, B> {
        RuntimeBuilder {
            myrmic: self,
            tags,
            name: uuid::Uuid::new_v4().to_string(),
            persistent: false,
            config: None,
            connect: vec![],
            listen: vec![],
        }
    }

    /// run: myrmic runtimes list
    pub async fn list_runtimes(&self) -> Result<Vec<String>, Error> {
        let output = self.run(&["runtimes", "list"]).await?;
        Ok(parse_runtime_list(&output.stdout))
    }

    /// run: myrmic runtimes delete `name`
    ///
    /// Returns once the runtime has exited: the CLI waits for that. A runtime that is not running
    /// fails with an [`Error::Cli`], like any other failed delete.
    pub async fn delete_runtime(&self, name: &str) -> Result<(), Error> {
        self.run(&["runtimes", "delete", name]).await?;
        Ok(())
    }

    /// run: myrmic new --name `name` [--sdk `sdk`] into a temporary directory on the target
    pub async fn new_cell(&self, name: &str, sdk: Option<&str>) -> Result<CellSpec<B>, Error> {
        let dir = TargetTempDir::create(self, name).await;
        let cell = CellSpec::Temporary(dir);
        let mut args = vec!["new"];
        if let Some(sdk) = sdk {
            args.extend(["--sdk", sdk]);
        }
        args.extend(["--name", name, path_arg(cell.as_path())]);
        self.run(&args).await?;
        Ok(cell)
    }

    /// run: myrmic send `sri` `command` [`payload` --raw]
    ///
    /// `payload` is delivered as-is, so the test encodes it in the handler's codec (e.g.
    /// `serde_json::to_vec` for the JSON default). The CLI only hands the command over and prints
    /// no response, so success carries no value.
    pub async fn send(
        &self,
        sri: &str,
        command: &str,
        payload: Option<&[u8]>,
    ) -> Result<(), Error> {
        let payload = payload.map(hex::encode);
        let mut args = vec!["send", sri, command];
        if let Some(payload) = &payload {
            args.extend([payload.as_str(), "--raw"]);
        }
        self.run(&args).await?;
        Ok(())
    }

    /// describe a `myrmic deploy` of `cell`; finish with [`CellDeployBuilder::deploy`]
    pub fn cell(&self, cell: impl Into<CellSpec<B>>) -> CellDeployBuilder<'_, B> {
        CellDeployBuilder {
            myrmic: self,
            cell: cell.into(),
            srn: format!("e2e-{}", uuid::Uuid::new_v4().simple()),
            tags: &[],
            platforms: None,
            init: None,
            policy: None,
        }
    }

    /// run: myrmic deploy `app-spec.yml`; the SRIs are defined inside the app spec
    ///
    /// Returns once every cell the CLI reports as deployed shows up in `myrmic cells status`.
    pub async fn deploy_app(&self, app_spec: impl AsRef<Path>) -> Result<DeployedApp<B>, Error> {
        let output = self.run(&["deploy", path_arg(app_spec.as_ref())]).await?;
        let name = deployed_app_name(&output.stderr);
        let sris = deployed_sris(&output.stderr);
        for sri in &sris {
            self.wait_until_deployed(sri, true).await?;
        }
        Ok(DeployedApp::new(self.clone(), name, sris, output.stderr))
    }

    /// whether `sri` is listed by `myrmic cells status`
    pub async fn is_sri_deployed(&self, sri: &str) -> Result<bool, Error> {
        let output = self.run(&["cells", "status"]).await?;
        Ok(output.stdout.lines().any(|line| line.contains(sri)))
    }

    /// run: myrmic telemetry set-db-retention `retention`
    ///
    /// Log records only reach a [`DebugListener`] while they are persisted.
    pub async fn set_db_retention(&self, retention: &str) -> Result<(), Error> {
        self.run(&["telemetry", "set-db-retention", retention])
            .await?;
        Ok(())
    }

    /// describe a `myrmic telemetry debug --json`; finish with [`DebugListenerBuilder::start`]
    pub fn debug_listener(&self) -> DebugListenerBuilder<'_, B> {
        DebugListenerBuilder {
            myrmic: self,
            id: None,
            level: None,
        }
    }

    /// Run `myrmic <args>`; an unsuccessful exit is [`Error::Cli`].
    async fn run(&self, args: &[&str]) -> Result<CommandOutput, Error> {
        let output = self.run_program(&self.binary, args).await;
        if output.success {
            Ok(output)
        } else {
            Err(Error::Cli {
                stderr: output.stderr,
            })
        }
    }

    /// Run `program args` on the target and capture its output. Panics when it cannot be
    /// started, or its wrapper failed: broken test infrastructure, not a result.
    async fn run_program(&self, program: &str, args: &[&str]) -> CommandOutput {
        let command = self.backend.command(program, args);
        let invocation = format!("{command:?}");
        let output = tokio::process::Command::from(command)
            .output()
            .await
            .unwrap_or_else(|err| panic!("failed to start {invocation}: {err}"));
        self.captured(&invocation, output)
            .unwrap_or_else(|stderr| panic!("{stderr}"))
    }

    /// Blocking run of `myrmic <args>` for `Drop` (see [`Self::run_program_blocking`]).
    fn run_blocking(&self, args: &[&str]) -> std::io::Result<CommandOutput> {
        self.run_program_blocking(&self.binary, args)
    }

    /// Blocking run of `program args` on the target for `Drop`, returning the raw output. A
    /// command that cannot be started, or whose wrapper failed, is the error instead of a panic:
    /// a panic during unwinding aborts the process and hides the original one.
    fn run_program_blocking(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        let mut command = self.backend.command(program, args);
        let invocation = format!("{command:?}");
        let output = command.output()?;
        self.captured(&invocation, output)
            .map_err(std::io::Error::other)
    }

    /// `output`, with the invocation and exit code folded into a failed command's stderr (the
    /// program's own message does not say which host or wrapper it came from), or that stderr as
    /// the error when the wrapper failed rather than the program.
    fn captured(
        &self,
        invocation: &str,
        output: std::process::Output,
    ) -> Result<CommandOutput, String> {
        let code = output.status.code();
        let mut output = CommandOutput::from(output);
        if !output.success {
            output.stderr = format!("{invocation} (exit {code:?}): {}", output.stderr);
        }
        // no exit code means killed by a signal, which is not a wrapper's exit status
        if code.is_some_and(|code| self.backend.wrapper_exit_codes().contains(&code)) {
            Err(output.stderr)
        } else {
            Ok(output)
        }
    }

    /// Wait until `sri` is `deployed` according to `myrmic cells status`, or no longer is.
    async fn wait_until_deployed(&self, sri: &str, deployed: bool) -> Result<(), Error> {
        let waited_for = if deployed {
            format!("SRI `{sri}` to show up in `cells status`")
        } else {
            format!("SRI `{sri}` to leave `cells status`")
        };
        wait_for(waited_for, || async {
            Ok(self.is_sri_deployed(sri).await? == deployed)
        })
        .await
    }
}

/// Poll `condition` until it holds, for at most [`crate::wait::DEFAULT_TIMEOUT`]. A failing poll
/// ends the wait with its error.
async fn wait_for<F, Fut>(waited_for: String, mut condition: F) -> Result<(), Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool, Error>>,
{
    let polled = tokio::time::timeout(crate::wait::DEFAULT_TIMEOUT, async {
        loop {
            if condition().await? {
                return Ok(());
            }
            tokio::time::sleep(crate::wait::DEFAULT_POLL_INTERVAL).await;
        }
    })
    .await;
    polled.unwrap_or(Err(Error::Timeout { waited_for }))
}

/// `path` as a CLI argument; arguments are strings so the ssh backend can quote them
fn path_arg(path: &Path) -> &str {
    path.to_str()
        .unwrap_or_else(|| panic!("{} is not UTF-8", path.display()))
}

/// the runtime names in `myrmic runtimes list` output (`<name>\t<details>` per line)
fn parse_runtime_list(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(runtime, _)| runtime.trim().to_owned())
        .collect()
}

/// the app name from `myrmic deploy <app-spec>`'s `deploying app (name = <name>)` line
fn deployed_app_name(stderr: &str) -> String {
    stderr
        .lines()
        .find_map(|line| {
            line.strip_prefix(INFO_PREFIX)?
                .strip_prefix("deploying app (name = ")?
                .strip_suffix(')')
        })
        .unwrap_or_else(|| {
            panic!("`myrmic deploy` reported no app name; did the spec deploy any cells?\n{stderr}")
        })
        .to_owned()
}

/// the SRIs from `myrmic deploy <app-spec>`'s `deployed cell (sri = <sri>, …)` lines
fn deployed_sris(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|line| {
            let (sri, _) = line
                .strip_prefix(INFO_PREFIX)?
                .strip_prefix("deployed cell (sri = ")?
                .split_once(',')?;
            Some(sri.to_owned())
        })
        .collect()
}

/// the zid from `myrmic runtimes start -d`'s `runtime "<name>" ready (<zid>)` line; `None` if
/// there is no such line or it holds no valid zid
fn started_runtime_zid(stderr: &str, name: &str) -> Option<zenoh::config::ZenohId> {
    let ready = format!("runtime {name:?} ready (");
    stderr.lines().find_map(|line| {
        line.strip_prefix(INFO_PREFIX)?
            .strip_prefix(&ready)?
            .strip_suffix(')')?
            .parse()
            .ok()
    })
}

/// Options for one `myrmic runtimes start`, created by [`Myrmic::runtime`].
///
/// Options are per start, not per [`Myrmic`]: one shim may start the same runtime temporary
/// first and persistent later.
pub struct RuntimeBuilder<'a, B> {
    myrmic: &'a Myrmic<B>,
    tags: &'a [&'a str],
    name: String,
    persistent: bool,
    config: Option<String>,
    connect: Vec<String>,
    listen: Vec<String>,
}

impl<B> RuntimeBuilder<'_, B>
where
    B: MyrmicBackend,
{
    /// `--name`; defaults to a random UUID. Set a fixed name for a runtime that is started again
    /// under the same identity (restarts, rack hosts).
    pub fn name(mut self, name: &str) -> Self {
        name.clone_into(&mut self.name);
        self
    }

    /// Whether the runtime keeps its database across restarts; defaults to `false`, i.e. `--tmp`
    /// is passed. `--tmp` forces an in-memory database and overrides a configured `db.directory`,
    /// so a case that restarts a runtime and expects its data back has to opt out. Temporary
    /// runtimes also keep the database directories of random names from piling up under
    /// `$XDG_DATA_HOME/myrmic`.
    pub fn persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// `--connect`: a peer endpoint the runtime connects to, merged with its config file's;
    /// repeatable
    pub fn connect(mut self, endpoint: &str) -> Self {
        self.connect.push(endpoint.to_owned());
        self
    }

    /// `--listen`: an endpoint the runtime listens on, merged with its config file's; repeatable
    pub fn listen(mut self, endpoint: &str) -> Self {
        self.listen.push(endpoint.to_owned());
        self
    }

    /// a config file for `myrmic runtimes start <path>`, already present on the target (e.g. to
    /// pin the zenoh listen port, or to set `db.directory`)
    pub fn config(mut self, path: &str) -> Self {
        self.config = Some(path.to_owned());
        self
    }

    /// run: myrmic runtimes start -d --name `name` [--tmp] [--tag `tag`...] [--connect
    /// `endpoint`...] [--listen `endpoint`...] [`config`]
    ///
    /// Returns once the runtime is ready: `runtimes start -d` returns only once cells can be placed
    /// on it.
    pub async fn start(self) -> Result<Runtime<B>, Error> {
        // `runtimes start` reads its config before it daemonizes, so the file may go once the
        // start returns
        let backend_config = match self.myrmic.backend.runtime_config() {
            Some(content) => {
                assert!(
                    self.config.is_none(),
                    "the backend sets the runtime config itself"
                );
                let dir = TargetTempDir::create(self.myrmic, "myrmic-runtime-config").await;
                let path = dir.write("runtime.yaml", &content).await;
                Some((dir, path))
            }
            None => None,
        };
        let mut args = vec!["runtimes", "start", "-d", "--name", &self.name];
        if !self.persistent {
            args.push("--tmp");
        }
        for tag in self.tags {
            args.extend(["--tag", tag]);
        }
        for endpoint in &self.connect {
            args.extend(["--connect", endpoint]);
        }
        for endpoint in &self.listen {
            args.extend(["--listen", endpoint]);
        }
        if let Some(config) = &self.config {
            args.push(config);
        }
        if let Some((_, path)) = &backend_config {
            args.push(path_arg(path));
        }
        let output = self.myrmic.run(&args).await?;
        let Some(zid) = started_runtime_zid(&output.stderr, &self.name) else {
            // The daemon runs, but there is no Runtime yet whose drop would delete it.
            let deleted = self.myrmic.delete_runtime(&self.name).await;
            panic!(
                "`myrmic runtimes start` reported no zid for `{}` (deleting it: {deleted:?})\n{}",
                self.name, output.stderr
            );
        };
        Ok(Runtime {
            myrmic: self.myrmic.clone(),
            name: self.name,
            zid,
            tags: self.tags.iter().map(|tag| (*tag).to_owned()).collect(),
        })
    }
}

/// Options for one `myrmic deploy` of a cell, created by [`Myrmic::cell`].
pub struct CellDeployBuilder<'a, B>
where
    B: MyrmicBackend,
{
    myrmic: &'a Myrmic<B>,
    cell: CellSpec<B>,
    srn: String,
    tags: &'a [&'a str],
    platforms: Option<&'a [PlatformFamily]>,
    init: Option<&'a [u8]>,
    policy: Option<RestartType>,
}

impl<'a, B> CellDeployBuilder<'a, B>
where
    B: MyrmicBackend,
{
    /// `--name`, the SRN the cell is deployed under; defaults to a random `e2e-<uuid>` (see
    /// [`DeployedCell::sri`])
    pub fn srn(mut self, srn: &str) -> Self {
        srn.clone_into(&mut self.srn);
        self
    }

    /// `--tag`s the runtime must carry; defaults to none
    pub fn tags(mut self, tags: &'a [&'a str]) -> Self {
        self.tags = tags;
        self
    }

    /// `--platform`: the platforms a cell crate is built for; defaults to the CLI's (`linux`)
    pub fn platforms(mut self, platforms: &'a [PlatformFamily]) -> Self {
        self.platforms = Some(platforms);
        self
    }

    /// `--init` with `--raw`: the arguments delivered as-is to the cell's `#[init]`, encoded in
    /// its codec (see [`Myrmic::send`]); defaults to none
    pub fn init(mut self, init: &'a [u8]) -> Self {
        self.init = Some(init);
        self
    }

    /// `--policy`: the cell's restart policy; defaults to the CLI's ([`RestartType::Never`])
    pub fn policy(mut self, policy: RestartType) -> Self {
        self.policy = Some(policy);
        self
    }

    /// run: myrmic deploy --name `srn` `cell` [--tag `tag`...] [--platform `platforms`]
    /// [--init `init` --raw] [--policy `policy`]
    ///
    /// Returns once the SRI shows up in `myrmic cells status`.
    pub async fn deploy(self) -> Result<DeployedCell<B>, Error> {
        let platforms = self.platforms.map(|platforms| {
            platforms
                .iter()
                .map(|platform| platform.name())
                .collect::<Vec<_>>()
                .join(",")
        });
        let init = self.init.map(hex::encode);
        let mut args = vec!["deploy", "--name", &self.srn, path_arg(self.cell.as_path())];
        for tag in self.tags {
            args.extend(["--tag", tag]);
        }
        if let Some(platforms) = &platforms {
            args.extend(["--platform", platforms.as_str()]);
        }
        if let Some(init) = &init {
            args.extend(["--init", init.as_str(), "--raw"]);
        }
        if let Some(policy) = self.policy {
            args.extend(["--policy", policy.spelling()]);
        }
        let output = self.myrmic.run(&args).await?;
        let sri = Sri::of_path(&self.srn)
            .expect("myrmic accepted an SRN that cannot be converted to an SRI")
            .to_string();
        self.myrmic.wait_until_deployed(&sri, true).await?;
        Ok(DeployedCell::new(self.myrmic.clone(), sri, output.stderr))
    }
}

/// How long [`DebugListenerBuilder::start`] waits before returning. The CLI reports nothing once
/// its subscriptions are in place (it prints `starting debug stream` before it even opens its
/// session), so the wait is a guess. Measured with [`Myrmic::local_isolated`]: without it, a
/// command sent right after the spawn was missed in most runs; with 5 s, none was.
pub const DEBUG_LISTENER_SETTLE: std::time::Duration = std::time::Duration::from_secs(5);

/// Options for one `myrmic telemetry debug --json`, created by [`Myrmic::debug_listener`].
pub struct DebugListenerBuilder<'a, B> {
    myrmic: &'a Myrmic<B>,
    id: Option<String>,
    level: Option<String>,
}

impl<B> DebugListenerBuilder<'_, B>
where
    B: MyrmicBackend,
{
    /// `--id`: only entries of this cell (SRI or SRN); defaults to every cell
    pub fn id(mut self, id: &str) -> Self {
        self.id = Some(id.to_owned());
        self
    }

    /// `--level`: raise the cell log level on all connected nodes for as long as the listener
    /// lives; dropping the listener has the CLI restore the previous filter. Defaults to leaving
    /// the level alone.
    pub fn level(mut self, level: &str) -> Self {
        self.level = Some(level.to_owned());
        self
    }

    /// run: myrmic telemetry debug --json [--id `id`] [--level `level`]
    ///
    /// Returns after [`DEBUG_LISTENER_SETTLE`], so the listener sees what happens after the
    /// return; start it before whatever it is meant to observe. The CLI keeps running for as long
    /// as the returned listener lives.
    pub async fn start(self) -> DebugListener {
        let mut args = vec!["telemetry", "debug", "--json"];
        if let Some(id) = &self.id {
            args.extend(["--id", id]);
        }
        if let Some(level) = &self.level {
            args.extend(["--level", level]);
        }
        let listener = DebugListener::spawn(self.myrmic, &args).await;
        tokio::time::sleep(DEBUG_LISTENER_SETTLE).await;
        listener
    }
}

/// a runtime is the outcome of `myrmic runtimes start`
///
/// Dropping a `Runtime` deletes it best-effort (panic-safe cleanup), also after an explicit
/// [`Runtime::delete`]: a runtime that is already gone counts as deleted. Call
/// [`Runtime::delete`] when the test asserts on the post-delete state.
pub struct Runtime<B>
where
    B: MyrmicBackend,
{
    myrmic: Myrmic<B>,
    name: String,
    zid: zenoh::config::ZenohId,
    tags: Vec<String>,
}

impl<B> Runtime<B>
where
    B: MyrmicBackend,
{
    /// Open a [`SorgHandle`] that waits for an exec runtime matching this
    /// runtime's tags and scopes all deploys to those same tags.
    pub async fn connect(&self, session: zenoh::Session) -> SorgHandle {
        let tag_refs: Vec<&str> = self.tags.iter().map(String::as_str).collect();
        SorgHandle::connect_with_tags(session, &tag_refs).await
    }

    /// the runtime name passed to `myrmic runtimes start --name`
    pub fn name(&self) -> &str {
        &self.name
    }

    /// the zenoh id the runtime runs under; kept across restarts under the same name
    pub fn zid(&self) -> zenoh::config::ZenohId {
        self.zid
    }

    /// run: myrmic runtimes delete; returns once the runtime has exited
    pub async fn delete(self) -> Result<(), Error> {
        self.myrmic.delete_runtime(&self.name).await
    }

    /// run: myrmic runtimes delete --signal kill; returns once the runtime has exited
    ///
    /// The runtime ends abruptly, as on a crash or power loss: it neither says goodbye to the
    /// swarm nor cleans up.
    pub async fn kill(self) -> Result<(), Error> {
        self.myrmic
            .run(&["runtimes", "delete", "--signal", "kill", &self.name])
            .await?;
        Ok(())
    }

    /// Freeze the runtime's process (`SIGSTOP`), as a stalled machine would: it keeps its state
    /// and connections but does nothing, timers and lease renewals included, until
    /// [`Self::resume`].
    pub async fn pause(&self) -> Result<(), Error> {
        self.signal("-STOP").await
    }

    /// let the process [`Self::pause`] froze run again (`SIGCONT`)
    pub async fn resume(&self) -> Result<(), Error> {
        self.signal("-CONT").await
    }

    /// Send `signal` to the runtime's process on the target. Its pid comes from `myrmic runtimes
    /// list`, so this works on every backend.
    async fn signal(&self, signal: &str) -> Result<(), Error> {
        let output = self.myrmic.run(&["runtimes", "list", &self.name]).await?;
        // `<name>\trunning\tpid=<pid>`
        let pid = output
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{}\trunning\tpid=", self.name)))
            .unwrap_or_else(|| {
                panic!(
                    "`runtimes list` shows no running `{}`: {}",
                    self.name, output.stdout
                )
            });
        let killed = self.myrmic.run_program("kill", &[signal, pid]).await;
        assert!(
            killed.success,
            "failed to send {signal} to `{}`: {}",
            self.name, killed.stderr
        );
        Ok(())
    }
}

impl<B> Drop for Runtime<B>
where
    B: MyrmicBackend,
{
    fn drop(&mut self) {
        match self
            .myrmic
            .run_blocking(&["runtimes", "delete", &self.name])
        {
            Ok(output) if output.success || already_gone(&output.stderr, &RUNTIME_GONE) => {}
            Ok(output) => eprintln!(
                "Runtime drop-guard: failed to delete runtime `{}`: {}",
                self.name, output.stderr
            ),
            Err(err) => eprintln!(
                "Runtime drop-guard: failed to run the delete of runtime `{}`: {err}",
                self.name
            ),
        }
    }
}
