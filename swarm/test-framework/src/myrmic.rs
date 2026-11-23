//! Shim around the myrmic CLI (runtimes, deploys, telemetry) running on the host, on a remote
//! host over SSH, or inside a docker container.
//!
//! The CLI does two different jobs for a test, and this module has a type for each:
//!
//! - [`Myrmic`] operates a swarm: it deploys cells and apps, sends commands, reads status and
//!   telemetry. It is the same whether or not it ever starts a runtime itself; a shim that only
//!   operates a swarm is common (e.g. [`crate::swarm3::Swarm::get_myrmic`]).
//! - [`StoppedRt`] and [`RunningRt`] are one runtime started with `myrmic runtimes start`, in
//!   its two states (see [`crate::runtime`] for how a test moves it between them). They come from
//!   [`Myrmic::runtime`], whose options (name, tags, `persistent`) the stopped state carries, so
//!   starting it again starts the same runtime. Which other runtimes it links to is not an option
//!   of its own: it is a member of an [`crate::overlay::Overlay`], which tells it at start.
//!
//! Both happen to go through the CLI, which is why a runtime handle holds a clone of the
//! [`Myrmic`] that created it: starting and stopping are `runtimes start` and `runtimes delete`
//! on the same shim, so on the same machine and in the same isolation.
//!
//! # Design notes
//!
//! The two jobs used to be one type: a shim started a runtime and the same shim was asked to
//! kill or restart it. They are separate because a runtime a test controls need not be a CLI
//! runtime (e.g. a node running inside the test process), while every swarm a test operates is
//! operated through the CLI. The runtime side is
//! therefore expressed as the [`crate::runtime`] traits, which [`StoppedRt`] and
//! [`RunningRt`] implement, and [`Myrmic`] stays the operator.
//!
//! [`MyrmicBackend`] requires `Send + Sync` because the runtime handles' transitions return `Send`
//! futures (see [`crate::runtime`]), and each handle holds a shim with its backend.

use std::path::Path;
use std::time::Duration;

pub use backend::{Docker, Local, MyrmicBackend, Ssh};
use cell_protocol::Sri;
pub use debug::{DebugEntry, DebugListener, DebugLog};
use myrmic_build::PlatformFamily;
use sorg_common::RestartType;

use crate::{
    CommandOutput,
    clients::sorg::SorgHandle,
    host,
    myrmic::cell::{CellSpec, DeployedApp, DeployedCell, TargetTempDir},
    overlay, runtime,
    wait::{DEFAULT_TIMEOUT, Timeout, wait_for},
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
    #[error(transparent)]
    Timeout(#[from] Timeout),
}

/// A shim around the myrmic CLI. It knows the CLI's commands and runs them on any
/// [`MyrmicBackend`]: on the host, on a remote host over SSH, or inside a container.
///
/// Every operation returns a [`Result`]; a test `.unwrap()`s or `.expect(..)`s what it assumes to
/// succeed. A command that cannot be started at all (binary, `ssh` or `docker` missing, host or
/// daemon unreachable) is broken test infrastructure, not a result, and panics.
///
/// A shim is attached to an [`crate::overlay::Overlay`] one way ([`overlay::Attachment`]): every CLI call
/// finds the runtimes that way (`--multicast-group` or `--connect`), and every runtime it starts
/// attaches that way, so the two can't drift apart. Runtimes of one machine attached two ways
/// come from two shims.
///
/// A shim comes from the host it runs on (see [`crate::host`]).
#[derive(Clone)]
pub struct Myrmic<B> {
    backend: B,
    /// the myrmic binary, as the backend's commands find it on the target
    binary: String,
    attachment: overlay::Attachment,
    /// how long each call waits for the runtimes to answer (see [`Self::timeout`])
    timeout: Duration,
}

impl<B> Myrmic<B>
where
    B: MyrmicBackend,
{
    /// A shim that runs `binary` (the myrmic binary, as `backend`'s commands find it on the
    /// target) through `backend`, attached by `attachment`; what a host hands out.
    pub(crate) fn new(backend: B, binary: String, attachment: overlay::Attachment) -> Self {
        Self {
            backend,
            binary,
            attachment,
            timeout: DEFAULT_CLI_TIMEOUT,
        }
    }

    /// The shim with every call waiting up to `timeout` for the runtimes to answer: the CLI's
    /// `--timeout`, which bounds both finding the runtimes and each request the call makes, e.g. a
    /// deploy's. [`DEFAULT_CLI_TIMEOUT`] by default.
    ///
    /// A deploy is answered once the runtime running the cell confirmed it, which the swarm's
    /// orchestration waits for up to its own timeout (see [`StoppedRt::orchestration`]); a
    /// call that should see the orchestration's own failure waits at least that long.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// where this shim's commands run
    pub fn get_backend(&self) -> &B {
        &self.backend
    }

    /// A runtime on this shim, not started yet, as a new member of the shim's overlay that scouts
    /// attached the way the shim is; start it with [`runtime::Stopped::start`].
    pub fn runtime(&self) -> StoppedRt<B> {
        StoppedRt {
            myrmic: self.clone(),
            ovl_member: self.attachment.add_member(self.backend.get_host_id()),
            tags: vec![],
            name: uuid::Uuid::new_v4().to_string(),
            persistent: false,
            telemetry: swarm_telemetry::TelemetryConfig::default(),
            execution: sorg_common::ExecConfig::default(),
            orchestration: swarm::OrchestrationConfig::default(),
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
        let attachment = self.attachment.get_cli_args(&self.backend.get_host_id());
        let timeout = timeout_arg(self.timeout);
        let output = self
            .run_program(&self.binary, &attached(&attachment, &timeout, args))
            .await;
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
        let attachment = self.attachment.get_cli_args(&self.backend.get_host_id());
        let timeout = timeout_arg(self.timeout);
        self.run_program_blocking(&self.binary, &attached(&attachment, &timeout, args))
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
        // a failing `cells status` ends the wait with its error
        wait_for(DEFAULT_TIMEOUT, &waited_for, || async {
            match self.is_sri_deployed(sri).await {
                Ok(is_deployed) => (is_deployed == deployed).then_some(Ok(())),
                Err(err) => Some(Err(err)),
            }
        })
        .await?
    }
}

/// How long a `myrmic` call waits for the runtimes to answer before it gives up (its
/// `--timeout`), unless the shim says otherwise ([`Myrmic::timeout`]). The CLI's own default,
/// 500 ms, is too short for a swarm across a VPN: a fresh session, and a reply through a router at
/// some 30 ms round trip, don't always fit in it. A runtime that answers sooner ends the wait
/// sooner, so a fast swarm loses nothing.
pub const DEFAULT_CLI_TIMEOUT: Duration = Duration::from_secs(5);

/// `timeout` as the value of the CLI's `--timeout`
fn timeout_arg(timeout: Duration) -> String {
    format!("{}ms", timeout.as_millis())
}

/// `args` of a `myrmic` call, behind `attachment`, the shim's
/// [`overlay::Attachment::get_cli_args`], by which the CLI finds the runtimes, and `timeout`, its
/// `--timeout` (see [`timeout_arg`])
fn attached<'a>(attachment: &'a [String; 2], timeout: &'a str, args: &[&'a str]) -> Vec<&'a str> {
    let mut attached: Vec<&str> = attachment.iter().map(String::as_str).collect();
    attached.extend(["--timeout", timeout]);
    attached.extend(args);
    attached
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

/// A runtime of `myrmic runtimes start` that is not running, created by [`Myrmic::runtime`] or
/// returned when a [`RunningRt`] stops. It holds what `runtimes start` is run with, so starting it
/// again brings back the same runtime: same name, so same zid.
///
/// Options are per runtime, not per [`Myrmic`]: one shim may start one runtime temporary and
/// another persistent.
#[derive(Clone)]
pub struct StoppedRt<B> {
    myrmic: Myrmic<B>,
    /// the runtime as a member of its overlay, which tells it its links at start
    ovl_member: overlay::Member,
    tags: Vec<String>,
    name: String,
    persistent: bool,
    telemetry: swarm_telemetry::TelemetryConfig,
    execution: sorg_common::ExecConfig,
    orchestration: swarm::OrchestrationConfig,
}

impl<B> StoppedRt<B>
where
    B: MyrmicBackend,
{
    /// `--tag` for each of `tags`; defaults to none
    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
        self
    }

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

    /// How the runtime exports its telemetry (logs, metrics, traces): where to, how often, how
    /// much; defaults to the runtime's own defaults.
    pub fn telemetry(mut self, telemetry: swarm_telemetry::TelemetryConfig) -> Self {
        self.telemetry = telemetry;
        self
    }

    /// How the runtime runs its cells (mailbox batch size, event buffer size, fuel, ...);
    /// defaults to the runtime's own defaults. Give the runtime its name and tags with
    /// [`Self::name`] and [`Self::tags`], not here: the CLI replaces a name set here, and adds
    /// tags set here to the runtime's.
    pub fn execution(mut self, execution: sorg_common::ExecConfig) -> Self {
        self.execution = execution;
        self
    }

    /// How the runtime's orchestration works, e.g. how long it waits for a runtime to confirm a
    /// cell it was told to run; defaults to the runtime's own defaults. Any runtime of a swarm may
    /// lead its orchestration, so a swarm that needs a setting gives it to every runtime.
    ///
    /// A client that should see the orchestration give up waits at least as long itself (see
    /// [`Myrmic::timeout`] and [`crate::swarm3::Swarm::sorg_query_timeout`]).
    pub fn orchestration(mut self, orchestration: swarm::OrchestrationConfig) -> Self {
        self.orchestration = orchestration;
        self
    }
}

impl<B> runtime::Stopped for StoppedRt<B>
where
    B: MyrmicBackend,
{
    type Error = Error;

    type Running = RunningRt<B>;

    /// run: myrmic runtimes start -d --name `name` [--tmp] [--tag `tag`...] `config`, where
    /// `config` sets what the overlay tells the runtime (the group it scouts on, the runtimes it
    /// connects to, the port it listens on), its telemetry, execution and orchestration settings
    ///
    /// Returns once the runtime is ready: `runtimes start -d` returns only once cells can be placed
    /// on it.
    async fn start(self) -> Result<RunningRt<B>, Error> {
        // JSON is YAML, which `runtimes start` parses
        let config = serde_json::json!({
            "zenoh": self.ovl_member.get_zenoh_settings(),
            "telemetry": self.telemetry,
            "execution": self.execution,
            "orchestration": self.orchestration,
        })
        .to_string();
        // `runtimes start` reads its config before it daemonizes, so the file may go once the
        // start returns
        let config_dir = TargetTempDir::create(&self.myrmic, "myrmic-runtime-config").await;
        let config_path = config_dir.write("runtime.yaml", &config).await;
        let mut args = vec!["runtimes", "start", "-d", "--name", &self.name];
        if !self.persistent {
            args.push("--tmp");
        }
        for tag in &self.tags {
            args.extend(["--tag", tag]);
        }
        args.push(path_arg(&config_path));
        let output = self.myrmic.run(&args).await?;
        let Some(zid) = started_runtime_zid(&output.stderr, &self.name) else {
            // The daemon runs, but there is no RunningRt yet whose drop would delete it.
            let deleted = self.myrmic.delete_runtime(&self.name).await;
            panic!(
                "`myrmic runtimes start` reported no zid for `{}` (deleting it: {deleted:?})\n{}",
                self.name, output.stderr
            );
        };
        // a CLI runtime is a zenoh peer
        let ovl_running = self.ovl_member.start_running(zenoh::config::WhatAmI::Peer);
        Ok(RunningRt {
            stopped: self,
            zid,
            _ovl_running: ovl_running,
        })
    }

    fn get_overlay_member(&self) -> &overlay::Member {
        &self.ovl_member
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
/// session), so the wait is a guess. Measured with [`crate::host::Local::myrmic_isolated`]: without it, a
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

/// A running runtime, the outcome of `myrmic runtimes start` (see [`StoppedRt`]).
///
/// Dropping a `RunningRt` deletes it best-effort (panic-safe cleanup), also after an explicit
/// [`runtime::Running::delete`]: a runtime that is already gone counts as deleted. Delete it
/// explicitly when the test asserts on the post-delete state.
pub struct RunningRt<B>
where
    B: MyrmicBackend,
{
    /// what it was started from, handed back when it stops
    stopped: StoppedRt<B>,
    zid: zenoh::config::ZenohId,
    /// records the runtime as running in its overlay while it exists; dropped after the
    /// runtime's delete in [`Drop::drop`]
    _ovl_running: overlay::RunningGuard,
}

impl<B> RunningRt<B>
where
    B: MyrmicBackend,
{
    /// Open a [`SorgHandle`] that waits for an exec runtime matching this
    /// runtime's tags and scopes all deploys to those same tags.
    pub async fn connect(&self, session: zenoh::Session) -> SorgHandle {
        let tag_refs: Vec<&str> = self.stopped.tags.iter().map(String::as_str).collect();
        SorgHandle::connect_with_tags(session, &tag_refs).await
    }

    /// the runtime name passed to `myrmic runtimes start --name`
    pub fn get_name(&self) -> &str {
        &self.stopped.name
    }

    /// End the runtime as on a crash or power loss: it neither says goodbye to the swarm nor
    /// cleans up. Returns once it is gone.
    ///
    /// run: myrmic runtimes delete --signal kill
    pub async fn kill(self) -> Result<StoppedRt<B>, Error> {
        self.stopped
            .myrmic
            .run(&["runtimes", "delete", "--signal", "kill", self.get_name()])
            .await?;
        Ok(self.stopped.clone())
    }

    /// Turn the handle of a runtime that is already gone, e.g. with its host's power, into a
    /// stopped one, without contacting the runtime or its host (the host may still be down).
    /// Starting the result starts the same runtime again.
    ///
    /// Dropping the running handle still runs `myrmic runtimes delete` on the target, which finds
    /// nothing left to delete; call it once the target is reachable again.
    pub fn mark_lost(self) -> StoppedRt<B> {
        self.stopped.clone()
    }

    /// Freeze the runtime, as a stalled machine would: it keeps its state and connections but
    /// does nothing, timers and lease renewals included, until [`Self::resume`]. `SIGSTOP` to the
    /// runtime's process.
    pub async fn pause(&self) -> Result<(), Error> {
        self.signal("-STOP").await
    }

    /// Let the runtime [`Self::pause`] froze run again: `SIGCONT` to its process.
    pub async fn resume(&self) -> Result<(), Error> {
        self.signal("-CONT").await
    }

    /// Send `signal` to the runtime's process on the target. Its pid comes from `myrmic runtimes
    /// list`, so this works on every backend.
    async fn signal(&self, signal: &str) -> Result<(), Error> {
        let name = self.get_name();
        let myrmic = &self.stopped.myrmic;
        let output = myrmic.run(&["runtimes", "list", name]).await?;
        // `<name>\trunning\tpid=<pid>`
        let pid = output
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}\trunning\tpid=")))
            .unwrap_or_else(|| {
                panic!(
                    "`runtimes list` shows no running `{name}`: {}",
                    output.stdout
                )
            });
        let killed = myrmic.run_program("kill", &[signal, pid]).await;
        assert!(
            killed.success,
            "failed to send {signal} to `{name}`: {}",
            killed.stderr
        );
        Ok(())
    }
}

impl RunningRt<Docker> {
    /// the host the runtime runs on, e.g. for its network's faults
    pub fn get_host(&self) -> &host::Docker {
        self.stopped.myrmic.get_backend().get_host()
    }
}

impl<B> runtime::Running for RunningRt<B>
where
    B: MyrmicBackend,
{
    type Error = Error;

    type Stopped = StoppedRt<B>;

    /// the zenoh id the runtime runs under; kept across restarts under the same name
    fn get_zid(&self) -> zenoh::config::ZenohId {
        self.zid
    }

    /// run: myrmic runtimes delete; returns once the runtime has exited
    async fn delete(self) -> Result<StoppedRt<B>, Error> {
        self.stopped.myrmic.delete_runtime(self.get_name()).await?;
        Ok(self.stopped.clone())
    }
}

impl<B> Drop for RunningRt<B>
where
    B: MyrmicBackend,
{
    fn drop(&mut self) {
        let name = self.get_name();
        match self
            .stopped
            .myrmic
            .run_blocking(&["runtimes", "delete", name])
        {
            Ok(output) if output.success || already_gone(&output.stderr, &RUNTIME_GONE) => {}
            Ok(output) => eprintln!(
                "Runtime drop-guard: failed to delete runtime `{name}`: {}",
                output.stderr
            ),
            Err(err) => {
                eprintln!(
                    "Runtime drop-guard: failed to run the delete of runtime `{name}`: {err}"
                );
            }
        }
    }
}
