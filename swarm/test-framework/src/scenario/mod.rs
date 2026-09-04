//! High-level scenario builder for the common "swarm process + sorg" e2e flow.

use std::{fmt::Debug, future::Future, path::PathBuf, time::Duration};

use cell_protocol::Sri;
use futures::future::BoxFuture;
use serde::de::DeserializeOwned;
use sorg_common::{CellInfeasibility, DeploymentError, RejectionReason, RequirementTags};
use uuid::Uuid;

use crate::cell::{AotCellArtifact, CellArtifact};
use crate::clients::sorg::{EventQueue, SorgHandle};
use crate::metrics::CellInteractionMetricsSnapshot;
use crate::myrmic::{BuildTarget, Myrmic};
use crate::swarm::{Swarm, SwarmProcess};

/// How a [`SwarmTestBuilder`] obtains its [`SwarmProcess`] before cells are built/loaded.
///
/// `Local` is the common case: spawn the swarm binary as a child process on this host from a
/// jsonnet config (see [`SwarmTestBuilder::config`]). `Custom` lets a scenario plug in a
/// different way to end up with a connected [`SwarmProcess`] — e.g. SSH-starting tagged myrmic
/// runtimes across a rack of hosts and opening a client session through a tunnel (see
/// [`SwarmTestBuilder::provisioner`]) — without this builder needing to know anything about how.
enum SpawnMode {
    Local(PathBuf),
    Custom(Box<dyn FnOnce() -> BoxFuture<'static, SwarmProcess> + Send>),
}

/// Expands `template` into `count` SRIs by substituting the `{n}` placeholder
/// with the 0-based replica index.
///
/// # Panics
///
/// Panics if `template` does not contain the `{n}` placeholder (which would
/// otherwise silently produce `count` identical SRIs).
pub fn sri_range(template: &str, count: usize) -> Vec<String> {
    assert!(
        template.contains("{n}"),
        "sri_range: template {template:?} does not contain the `{{n}}` placeholder, but count is {count}"
    );

    (0..count)
        .map(|n| template.replace("{n}", &n.to_string()))
        .collect()
}

/// Builder for the common e2e flow: spawn a swarm process, build/register cells,
/// connect a sorg handle, load the cells.
///
/// Cells are loaded during [`SpawnedSwarmTest::connect`] / [`SwarmTestBuilder::start`],
/// so event subscriptions created on the returned [`SwarmTestCtx`] only observe
/// events emitted afterwards. Tests that must subscribe before a cell is loaded
/// (e.g. load-time events) should connect with [`SpawnedSwarmTest::connect_deferred`]
/// and call [`SwarmTestCtx::load_cells`] once subscribed.
pub struct SwarmTest;

impl SwarmTest {
    /// start describing a scenario; finish with [`SwarmTestBuilder::start`] or
    /// [`SwarmTestBuilder::spawn`]
    pub fn builder() -> SwarmTestBuilder {
        SwarmTestBuilder::default()
    }
}

/// How long [`SwarmTestBuilder::spawn`] waits for the freshly spawned swarm's datalayer to come
/// up before it starts registering classes.
const DATALAYER_TIMEOUT: Duration = Duration::from_secs(30);

/// A cell that has been registered as a class and is waiting to be deployed.
struct PendingLoad {
    class_name: String,
    sri: String,
    /// Runtime the cell must land on. `None` means the scenario's own [`SwarmTestBuilder::tags`].
    tags: Option<Vec<String>>,
    /// Init arguments delivered to the cell's `#[init]` via the deployment command. `None` for
    /// cells whose init takes no payload.
    payload: Option<Vec<u8>>,
}

/// A cell the scenario will register, before it has been built.
enum PendingCell {
    /// built with the myrmic CLI from a cell directory, and loaded under each of these SRIs
    /// (more than one for [`SwarmTestBuilder::wasm_cell_replicated`])
    WasmPath(PathBuf, Vec<String>, BuildTarget),
    /// like `WasmPath`, but each `(sri, tags)` pair pins that replica to its own runtime tags
    /// instead of the scenario's own [`SwarmTestBuilder::tags`] — for topologies where each
    /// replica must land on a specific host (see
    /// [`SwarmTestBuilder::wasm_cell_replicated_pinned_with_api`]).
    WasmPathPinned(PathBuf, Vec<(String, Vec<String>)>, BuildTarget),
    /// already-built wasm module, optionally pinned to a runtime
    WasmArtifact(CellArtifact, String, Option<Vec<String>>),
    /// already-built AOT artifact, with optional `#[init]` arguments
    Aot(AotCellArtifact, String, Option<Vec<u8>>),
}

/// Accumulates the scenario configuration for a [`SwarmTest`] (config, cells, tags).
///
/// Cells are registered and loaded in the order they were declared, whatever their kind, so a
/// scenario can rely on e.g. a receiver being live before its sender.
#[derive(Default)]
pub struct SwarmTestBuilder {
    spawn_mode: Option<SpawnMode>,
    cells: Vec<PendingCell>,
    tags: Vec<String>,
    exec_runtime_timeout: Option<Duration>,
    query_timeout: Option<Duration>,
}

impl SwarmTestBuilder {
    /// swarm jsonnet config to spawn locally (use `asset!(..)`).
    ///
    /// Mutually exclusive with [`Self::provisioner`] — whichever is called last wins.
    pub fn config(mut self, path: impl Into<PathBuf>) -> Self {
        self.spawn_mode = Some(SpawnMode::Local(path.into()));
        self
    }

    /// Provide a custom way to obtain the connected [`SwarmProcess`] this scenario runs against,
    /// instead of spawning a local swarm process from a jsonnet config (see [`Self::config`]).
    ///
    /// Use this for deployments where "spawn one local process" doesn't apply — e.g. SSH-starting
    /// tagged myrmic runtimes across a set of remote hosts and opening a client zenoh session into
    /// that mesh. Everything downstream of [`Self::spawn`] (cell build/register/load, metrics,
    /// teardown) works identically regardless of how the [`SwarmProcess`] was obtained, since it
    /// only ever touches the process's zenoh session.
    ///
    /// Mutually exclusive with [`Self::config`] — whichever is called last wins.
    pub fn provisioner<F, Fut>(mut self, provision: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = SwarmProcess> + Send + 'static,
    {
        self.spawn_mode = Some(SpawnMode::Custom(Box::new(move || {
            Box::pin(provision()) as BoxFuture<'static, SwarmProcess>
        })));
        self
    }

    /// build the cell at `cell_path` with the myrmic CLI, register it, and load
    /// it under `sri` once connected
    pub fn wasm_cell(mut self, cell_path: impl Into<PathBuf>, sri: impl Into<String>) -> Self {
        self.cells.push(PendingCell::WasmPath(
            cell_path.into(),
            vec![sri.into()],
            BuildTarget::Wasm,
        ));
        self
    }

    /// like [`Self::wasm_cell`], but also generates the cell's `<name>-api.yml` (`myrmic build`
    /// with `BuildTarget::WasmWithApi`) — use this for a cell that a sibling cell resolves via
    /// `import_cells!`. Cells are built sequentially in the order they were pushed onto this
    /// builder, so push a cell before any sibling that imports its API.
    pub fn wasm_cell_with_api(
        mut self,
        cell_path: impl Into<PathBuf>,
        sri: impl Into<String>,
    ) -> Self {
        self.cells.push(PendingCell::WasmPath(
            cell_path.into(),
            vec![sri.into()],
            BuildTarget::WasmWithApi,
        ));
        self
    }

    /// build the cell at `cell_path` with the myrmic CLI once, register it, and
    /// load it `count` times, once for each SRI produced by expanding
    /// `sri_template` (see [`sri_range`]) once connected.
    pub fn wasm_cell_replicated(
        mut self,
        cell_path: impl Into<PathBuf>,
        sri_template: impl Into<String>,
        count: usize,
    ) -> Self {
        let sris = sri_range(&sri_template.into(), count);
        self.cells.push(PendingCell::WasmPath(
            cell_path.into(),
            sris,
            BuildTarget::Wasm,
        ));
        self
    }

    /// like [`Self::wasm_cell_replicated`], but also generates the cell's `<name>-api.yml`
    /// (`myrmic build` with `BuildTarget::WasmWithApi`) — use this for a cell that a sibling
    /// cell resolves via `import_cells!`. Cells are built sequentially in the order they were
    /// pushed onto this builder, so push a cell before any sibling that imports its API.
    pub fn wasm_cell_replicated_with_api(
        mut self,
        cell_path: impl Into<PathBuf>,
        sri_template: impl Into<String>,
        count: usize,
    ) -> Self {
        let sris = sri_range(&sri_template.into(), count);
        self.cells.push(PendingCell::WasmPath(
            cell_path.into(),
            sris,
            BuildTarget::WasmWithApi,
        ));
        self
    }

    /// like [`Self::wasm_cell_with_api`], but pinned to `tags` rather than the scenario's own
    /// [`Self::tags`] — the single-cell counterpart of
    /// [`Self::wasm_cell_replicated_pinned_with_api`], for a topology's one-off cell (e.g. a
    /// central/aggregator tier) that still needs to land on a specific host.
    pub fn wasm_cell_pinned_with_api(
        mut self,
        cell_path: impl Into<PathBuf>,
        sri: impl Into<String>,
        tags: Vec<String>,
    ) -> Self {
        self.cells.push(PendingCell::WasmPathPinned(
            cell_path.into(),
            vec![(sri.into(), tags)],
            BuildTarget::WasmWithApi,
        ));
        self
    }

    /// like [`Self::wasm_cell_replicated_with_api`], but each replica is pinned to its own set of
    /// runtime tags (one entry of `tags_per_replica` per replica, in order) rather than the
    /// scenario's own [`Self::tags`] — for topologies where each replica must land on a specific
    /// host/runtime (e.g. one Raspberry Pi per replica) instead of being load-balanced across
    /// whatever runtimes match the scenario's tags.
    pub fn wasm_cell_replicated_pinned_with_api(
        mut self,
        cell_path: impl Into<PathBuf>,
        sri_template: impl Into<String>,
        tags_per_replica: &[Vec<String>],
    ) -> Self {
        let sris = sri_range(&sri_template.into(), tags_per_replica.len());
        let pinned = sris
            .into_iter()
            .zip(tags_per_replica.iter().cloned())
            .collect();
        self.cells.push(PendingCell::WasmPathPinned(
            cell_path.into(),
            pinned,
            BuildTarget::WasmWithApi,
        ));
        self
    }

    /// register an already-built wasm artifact and load it under `sri` once connected.
    ///
    /// Unlike [`Self::wasm_cell`] this needs no myrmic binary on the host — the caller has
    /// already produced the module.
    pub fn wasm_artifact(mut self, artifact: CellArtifact, sri: impl Into<String>) -> Self {
        self.cells
            .push(PendingCell::WasmArtifact(artifact, sri.into(), None));
        self
    }

    /// [`Self::wasm_artifact`] pinned to `tags` rather than the scenario's own
    /// [`Self::tags`] — for scenarios spanning two runtimes, e.g. an embedded device
    /// alongside the swarm's own Linux exec.
    pub fn wasm_artifact_on(
        mut self,
        artifact: CellArtifact,
        sri: impl Into<String>,
        tags: &[&str],
    ) -> Self {
        let tags = tags.iter().map(std::string::ToString::to_string).collect();
        self.cells
            .push(PendingCell::WasmArtifact(artifact, sri.into(), Some(tags)));
        self
    }

    /// register a pre-built AOT artifact and load it under `sri` once connected
    pub fn aot_cell(mut self, artifact: AotCellArtifact, sri: impl Into<String>) -> Self {
        self.cells
            .push(PendingCell::Aot(artifact, sri.into(), None));
        self
    }

    /// [`Self::aot_cell`] delivering `payload` to the cell's `#[init]` via the deployment command,
    /// as a root/CLI deploy would. Use to seed a cell's initial state without running its default
    /// init path.
    pub fn aot_cell_with_payload(
        mut self,
        artifact: AotCellArtifact,
        sri: impl Into<String>,
        payload: Vec<u8>,
    ) -> Self {
        self.cells
            .push(PendingCell::Aot(artifact, sri.into(), Some(payload)));
        self
    }

    /// tags the sorg connection waits for on exec runtime discovery, and that
    /// scope all cell loads that do not name a runtime of their own
    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(std::string::ToString::to_string).collect();
        self
    }

    /// how long [`SpawnedSwarmTest::connect`] waits for an exec runtime matching [`Self::tags`]
    /// to register itself. Defaults to
    /// [`DEFAULT_EXEC_RUNTIME_TIMEOUT`](crate::clients::sorg::DEFAULT_EXEC_RUNTIME_TIMEOUT),
    /// which assumes the runtime comes up with the swarm process; raise it when the runtime is a
    /// physical device that still has to boot and join the network.
    pub fn exec_runtime_timeout(mut self, timeout: Duration) -> Self {
        self.exec_runtime_timeout = Some(timeout);
        self
    }

    /// how long deploys may take before the sorg client gives up
    /// (see [`SorgHandle::with_query_timeout`]).
    pub fn query_timeout(mut self, timeout: Duration) -> Self {
        self.query_timeout = Some(timeout);
        self
    }

    /// spawn the swarm process and register all cell artifacts, but do not
    /// connect yet — for tests that must act in between (e.g. flash a device
    /// that provides the exec runtime)
    pub async fn spawn(self) -> SpawnedSwarmTest {
        let spawn_mode = self
            .spawn_mode
            .expect("SwarmTest::builder(): config() or provisioner() is required");
        let process = match spawn_mode {
            SpawnMode::Local(config) => Swarm::local().spawn(&config).await,
            SpawnMode::Custom(provision) => provision().await,
        };

        // Registering the classes below is a datalayer write, so the DB plugin has to be up.
        process.wait_for_datalayer(DATALAYER_TIMEOUT).await;

        // resolve the myrmic binary only if something needs building — scenarios that pass
        // pre-built artifacts (embedded) must not require a myrmic binary on the host
        let needs_myrmic = self.cells.iter().any(|c| {
            matches!(
                c,
                PendingCell::WasmPath(..) | PendingCell::WasmPathPinned(..)
            )
        });
        let myrmic = needs_myrmic.then(Myrmic::local);

        let mut loads = Vec::new();
        // Cells are built sequentially in the order they were pushed, so a cell built with
        // WasmWithApi (see wasm_cell_with_api/wasm_cell_replicated_with_api) must be pushed
        // before any sibling that `import_cells!`s its generated `<name>-api.yml`.
        for cell in self.cells {
            match cell {
                PendingCell::WasmPath(cell_path, sris, target) => {
                    let artifact = myrmic
                        .as_ref()
                        .expect("myrmic resolved when a wasm_cell is declared")
                        .build(cell_path, target)
                        .await;
                    artifact.register_on(&process).await;
                    for sri in sris {
                        loads.push(PendingLoad {
                            class_name: artifact.name.clone(),
                            sri,
                            tags: None,
                            payload: None,
                        });
                    }
                }
                PendingCell::WasmPathPinned(cell_path, pinned, target) => {
                    let artifact = myrmic
                        .as_ref()
                        .expect("myrmic resolved when a wasm_cell is declared")
                        .build(cell_path, target)
                        .await;
                    artifact.register_on(&process).await;
                    for (sri, tags) in pinned {
                        loads.push(PendingLoad {
                            class_name: artifact.name.clone(),
                            sri,
                            tags: Some(tags),
                            payload: None,
                        });
                    }
                }
                PendingCell::WasmArtifact(artifact, sri, tags) => {
                    artifact.register_on(&process).await;
                    loads.push(PendingLoad {
                        class_name: artifact.name,
                        sri,
                        tags,
                        payload: None,
                    });
                }
                PendingCell::Aot(artifact, sri, payload) => {
                    artifact.register_on(&process).await;
                    loads.push(PendingLoad {
                        class_name: artifact.name,
                        sri,
                        tags: None,
                        payload,
                    });
                }
            }
        }

        SpawnedSwarmTest {
            process,
            loads,
            tags: self.tags,
            exec_runtime_timeout: self.exec_runtime_timeout,
            query_timeout: self.query_timeout,
        }
    }

    /// [`Self::spawn`] + [`SpawnedSwarmTest::connect`]
    pub async fn start(self) -> SwarmTestCtx {
        self.spawn().await.connect().await
    }
}

/// A running swarm process with cells registered but not yet loaded.
pub struct SpawnedSwarmTest {
    process: SwarmProcess,
    loads: Vec<PendingLoad>,
    tags: Vec<String>,
    exec_runtime_timeout: Option<Duration>,
    query_timeout: Option<Duration>,
}

impl SpawnedSwarmTest {
    /// the running swarm process
    pub fn process(&self) -> &SwarmProcess {
        &self.process
    }

    /// connect a sorg handle (waits for a matching exec runtime) and load all cells
    pub async fn connect(self) -> SwarmTestCtx {
        let mut ctx = self.connect_deferred().await;
        ctx.load_cells().await;
        ctx
    }

    /// [`Self::connect`] without loading the cells, so the test can subscribe to events a cell
    /// emits while loading. Finish with [`SwarmTestCtx::load_cells`].
    pub async fn connect_deferred(self) -> SwarmTestCtx {
        let tag_refs: Vec<&str> = self.tags.iter().map(std::string::String::as_str).collect();
        let exec_timeout = self.exec_runtime_timeout;
        let mut sorg = match exec_timeout {
            Some(timeout) => {
                self.process
                    .connect_sorg_with_tags_timeout(&tag_refs, timeout)
                    .await
            }
            None => self.process.connect_sorg_with_tags(&tag_refs).await,
        };
        if let Some(timeout) = self.query_timeout {
            sorg = sorg.with_query_timeout(timeout);
        }

        // Cells pinned to another runtime need that runtime registered too, or their deploy
        // races the registration and fails with `NoRuntimesAvailable`.
        for extra in extra_runtime_tags(&self.loads, &self.tags) {
            let refs: Vec<&str> = extra.iter().map(std::string::String::as_str).collect();
            sorg.wait_for_runtime(
                &refs,
                exec_timeout.unwrap_or(crate::clients::sorg::DEFAULT_EXEC_RUNTIME_TIMEOUT),
            )
            .await;
        }

        SwarmTestCtx {
            process: self.process,
            sorg,
            loads: self.loads,
            default_tags: self.tags,
        }
    }
}

/// The distinct runtime tag-sets named by `loads` that differ from the scenario's own `tags`.
fn extra_runtime_tags(loads: &[PendingLoad], tags: &[String]) -> Vec<Vec<String>> {
    let mut extra: Vec<Vec<String>> = Vec::new();
    for load in loads {
        let Some(load_tags) = &load.tags else {
            continue;
        };
        if load_tags == tags || extra.contains(load_tags) {
            continue;
        }
        extra.push(load_tags.clone());
    }
    extra
}

/// Why [`SwarmTestCtx::wait_for_completeness`] returned — not just whether it's complete, but
/// what to do about it if not: wait longer next time, or accept that waiting won't help.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Completeness {
    /// every expected command was received and every expected event actually arrived.
    Complete,
    /// `max_wait` ran out without positive evidence of permanent loss — either `commands_received`
    /// never stopped changing (no independent target exists to check it against, so a stall here
    /// would look identical to a genuine slow ramp-up), or `events_received` was still climbing
    /// towards its known target. Either way: a longer `drain_timeout` might resolve it, so this is
    /// reported the same regardless of which phase ran out of time.
    TimedOut,
    /// `events_received` stopped making any progress towards the known `events_sent` target,
    /// short of it, and stayed stopped for `stable_rounds` consecutive samples. Permanent: a row
    /// that's become invisible to a mailbox's cursor (see the db-layer bug documented on
    /// [`SwarmTestCtx::wait_for_completeness`]) never starts moving again, so waiting longer next
    /// time won't help — this is signal that a row was actually lost, not that the wait was cut
    /// short. Unlike `TimedOut`, this is positive evidence of a real, permanent loss.
    Stalled,
}

impl Completeness {
    pub fn is_complete(self) -> bool {
        self == Self::Complete
    }

    /// A short, human-readable explanation of what this outcome means and what (if anything) to
    /// do about it — shared between the harness's console warning and the PDF report, so both say
    /// the same thing.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::Complete => "every expected command and event was accounted for.",
            Self::Stalled => {
                "processing permanently stalled short of the expected event count — some command(s)/event(s) were genuinely lost, not just still draining. A longer drain_timeout will not fix this."
            }
            Self::TimedOut => {
                "processing was still draining when drain_timeout ran out — the numbers below may still be catching up; try a longer drain_timeout."
            }
        }
    }
}

/// A fully wired scenario: swarm process up, cells loaded, sorg connected.
/// Dropping the ctx tears the process down.
pub struct SwarmTestCtx {
    process: SwarmProcess,
    sorg: SorgHandle,
    loads: Vec<PendingLoad>,
    default_tags: Vec<String>,
}

impl SwarmTestCtx {
    /// the running swarm process
    pub fn process(&self) -> &SwarmProcess {
        &self.process
    }

    /// the connected sorg handle
    pub fn sorg(&mut self) -> &mut SorgHandle {
        &mut self.sorg
    }

    /// Deploy every cell the scenario registered but has not loaded yet, panicking on failure.
    ///
    /// `NoRuntimesAvailable` specifically is retried with backoff before the panic: the
    /// orchestrator's placement read routes through its *own* client, and under pass-boundary
    /// churn that read can land on an empty or behind `sys` replica (a drowned locate falls back
    /// to `any_node`) even while the driver-side registry barrier has just verified every
    /// runtime registered and leased — an empty view moments after provisioning is far more
    /// likely a read-placement artifact than truth. [`Self::try_load_cells`] keeps the failed
    /// cell (and everything after it) queued, so a retry resumes exactly where it stopped.
    pub async fn load_cells(&mut self) {
        const ATTEMPTS: u32 = 8;

        for attempt in 1..=ATTEMPTS {
            match self.try_load_cells().await {
                Ok(()) => return,
                Err(DeploymentError::NoRuntimesAvailable) if attempt < ATTEMPTS => {
                    eprintln!(
                        "deploy hit NoRuntimesAvailable (attempt {attempt}/{ATTEMPTS}); retrying \
                         — placement's registry read likely landed on a stale replica"
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                // Same argument, different symptom: `wait_for_class_visible`
                // proves *some* holder has the class, while placement's own
                // read routes independently and may land on one that does not
                // yet. That divergence used to be impossible — every equal-head
                // read in the mesh resolved to the same node — and became
                // possible once those ties were spread. A runtime that is
                // eligible on every other count and only lacks the artifact is
                // a read that arrived early, not a deployment that cannot work.
                Err(DeploymentError::Infeasible(ref cells))
                    if attempt < ATTEMPTS && Self::only_missing_artifacts(cells) =>
                {
                    eprintln!(
                        "deploy hit MissingArtifact (attempt {attempt}/{ATTEMPTS}); retrying \
                         — placement's class read likely landed on a stale replica"
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Err(err) => panic!("failed to load scenario cells: {err:?}"),
            }
        }
    }

    /// Whether every unplaceable cell was blocked only by a missing artifact —
    /// i.e. some runtime was otherwise eligible and merely could not see the
    /// class yet. A cell no runtime has the tags for is a real configuration
    /// error and must not be retried into a timeout.
    fn only_missing_artifacts(cells: &[CellInfeasibility]) -> bool {
        !cells.is_empty()
            && cells.iter().all(|cell| {
                cell.rejections
                    .iter()
                    .any(|r| matches!(r.reason, RejectionReason::MissingArtifact(_)))
            })
    }

    /// [`Self::load_cells`] returning the orchestrator's error instead of panicking.
    ///
    /// Stops at the first failure: cells already loaded stay loaded, and the one that failed
    /// plus everything after it stay queued, so the test can retry with another
    /// [`Self::try_load_cells`].
    pub async fn try_load_cells(&mut self) -> Result<(), DeploymentError> {
        // Registration wrote each class through one routed transaction; the deploy below reads
        // it back through another, and nothing guarantees the write has converged to whichever
        // node the placement read lands on — see `SwarmProcess::wait_for_class_visible` for the
        // failures this barrier exists to prevent. Checked per distinct class, not per cell:
        // replicated cells share a class, and a class already visible costs one read.
        let mut classes: Vec<String> = self
            .loads
            .iter()
            .map(|load| load.class_name.clone())
            .collect();
        classes.sort_unstable();
        classes.dedup();
        for class in &classes {
            self.process.wait_for_class_visible(class).await;
        }

        while let Some(load) = self.loads.first() {
            let tags = load
                .tags
                .clone()
                .unwrap_or_else(|| self.default_tags.clone());
            self.sorg
                .try_load_cell_with_tags_and_args(
                    &load.class_name,
                    &load.sri,
                    &RequirementTags::new(tags.into_iter().collect()),
                    load.payload.clone(),
                )
                .await?;
            self.loads.remove(0);
        }
        Ok(())
    }

    /// Register `sri` for a later [`Self::load_cells`], as [`SwarmTestBuilder::aot_cell`] would.
    /// Use to re-deploy a cell whose class is already registered (e.g. after an undeploy).
    pub fn queue_load(&mut self, class_name: impl Into<String>, sri: impl Into<String>) {
        self.loads.push(PendingLoad {
            class_name: class_name.into(),
            sri: sri.into(),
            tags: None,
            payload: None,
        });
    }

    /// shorthand for [`SorgHandle::undeploy_cell`]
    pub async fn undeploy_cell(&self, sri: &str) {
        self.sorg.undeploy_cell(sri).await;
    }

    /// shorthand for [`SorgHandle::create_instance`]
    pub async fn create_instance(&self, sri: &str, class_name: &str, key: &str, state: Vec<u8>) {
        self.sorg.create_instance(sri, class_name, key, state).await;
    }

    /// a cheaply-cloneable, owned handle to the connected sorg session — for callers (e.g.
    /// [`crate::producers::command::LoadProducer`]) that need to send commands from spawned tasks
    pub fn sorg_handle(&self) -> SorgHandle {
        self.sorg.clone()
    }

    /// shorthand for [`SorgHandle::subscribe_cell_event`]
    pub async fn subscribe_cell_event(&mut self, event: &str) -> EventQueue {
        self.sorg.subscribe_cell_event(event).await
    }

    /// shorthand for [`SorgHandle::publish_cell_event`]
    pub async fn publish_cell_event(&self, event: &str, payload: Vec<u8>) {
        self.sorg.publish_cell_event(event, payload).await;
    }

    /// shorthand for [`SorgHandle::command_send`] taking the SRI as a string
    pub async fn command_send(&self, sri: &str, cmd_name: &str, payload: Option<Vec<u8>>) {
        self.sorg
            .command_send(
                Sri::of_path(sri).expect("invalid cell sri"),
                cmd_name,
                payload,
            )
            .await;
    }

    /// shorthand for [`SorgHandle::get_cell_state`] taking the SRI as a string
    pub async fn get_cell_state<S: DeserializeOwned>(&self, sri: &str, key: &str) -> Option<S> {
        self.sorg
            .get_cell_state(Sri::of_path(sri).expect("invalid cell sri"), key)
            .await
    }

    /// Opens a fresh [`DbHandle`](crate::clients::db::DbHandle) on this scenario's zenoh
    /// session — used by the `query_*`/`await_*`/`cell_*`/`event_*`/`command_backlog`
    /// shorthands below.
    fn db(&self) -> crate::clients::db::DbHandle {
        crate::clients::db::DbHandle::new(self.sorg.session())
    }

    /// shorthand for [`crate::clients::db::DbHandle::query_logs_for_trace`], opening a fresh
    /// [`DbHandle`](crate::clients::db::DbHandle) on this scenario's zenoh session
    pub async fn query_logs_for_trace(
        &self,
        trace_id: Uuid,
    ) -> Vec<swarm_telemetry::db::opentelemetry_proto::tonic::logs::v1::LogRecord> {
        if let Some(files) = self.process.telemetry_files() {
            return files
                .logs()
                .await
                .into_iter()
                .filter(|record| {
                    crate::clients::db::trace_id_of(&record.trace_id) == Some(trace_id)
                })
                .collect();
        }
        self.db().query_logs_for_trace(trace_id).await
    }

    pub async fn query_spans(
        &self,
        trace_id: Uuid,
    ) -> Vec<swarm_telemetry::db::opentelemetry_proto::tonic::trace::v1::Span> {
        if let Some(files) = self.process.telemetry_files() {
            return files
                .spans()
                .await
                .into_iter()
                .filter(|span| crate::clients::db::trace_id_of(&span.trace_id) == Some(trace_id))
                .collect();
        }
        self.db().query_spans(trace_id).await
    }

    /// shorthand for [`crate::clients::db::DbHandle::query_spans_by_name`], opening a fresh
    /// [`DbHandle`](crate::clients::db::DbHandle) on this scenario's zenoh session
    pub async fn query_spans_by_name(
        &self,
        name: &str,
    ) -> Vec<swarm_telemetry::db::opentelemetry_proto::tonic::trace::v1::Span> {
        if let Some(files) = self.process.telemetry_files() {
            return files
                .spans()
                .await
                .into_iter()
                .filter(|span| span.name == name)
                .collect();
        }
        self.db().query_spans_by_name(name).await
    }

    /// shorthand for [`crate::clients::db::DbHandle::query_spans_for_traces`], opening a fresh
    /// [`DbHandle`](crate::clients::db::DbHandle) on this scenario's zenoh session
    pub async fn query_spans_for_traces(
        &self,
        trace_ids: &[Uuid],
    ) -> std::collections::HashMap<
        Uuid,
        Vec<swarm_telemetry::db::opentelemetry_proto::tonic::trace::v1::Span>,
    > {
        if let Some(files) = self.process.telemetry_files() {
            let wanted: std::collections::HashSet<Uuid> = trace_ids.iter().copied().collect();
            let mut grouped: std::collections::HashMap<_, Vec<_>> =
                std::collections::HashMap::new();
            for span in files.spans().await {
                if let Some(trace_id) =
                    crate::clients::db::trace_id_of(&span.trace_id).filter(|id| wanted.contains(id))
                {
                    grouped.entry(trace_id).or_default().push(span);
                }
            }
            return grouped;
        }
        self.db().query_spans_for_traces(trace_ids).await
    }

    /// shorthand for [`crate::clients::db::DbHandle::query_spans_grouped_since`], opening a
    /// fresh [`DbHandle`](crate::clients::db::DbHandle) on this scenario's zenoh session
    pub async fn query_spans_grouped_since(
        &self,
        since_unix_nanos: u64,
    ) -> std::collections::HashMap<
        Uuid,
        Vec<swarm_telemetry::db::opentelemetry_proto::tonic::trace::v1::Span>,
    > {
        if let Some(files) = self.process.telemetry_files() {
            let mut grouped: std::collections::HashMap<_, Vec<_>> =
                std::collections::HashMap::new();
            for span in files.spans().await {
                if span.start_time_unix_nano < since_unix_nanos {
                    continue;
                }
                if let Some(trace_id) = crate::clients::db::trace_id_of(&span.trace_id) {
                    grouped.entry(trace_id).or_default().push(span);
                }
            }
            return grouped;
        }
        self.db().query_spans_grouped_since(since_unix_nanos).await
    }

    pub async fn await_span_insertion(&self) {
        self.db().await_span_insertion().await;
    }

    /// shorthand for [`crate::clients::db::DbHandle::await_span_hops`], opening a fresh
    /// [`DbHandle`](crate::clients::db::DbHandle) on this scenario's zenoh session
    pub async fn await_span_hops(
        &self,
        trace_id: Uuid,
        expected_sris: &[Vec<Sri>],
        max_attempts: u32,
    ) -> (
        bool,
        Vec<swarm_telemetry::db::opentelemetry_proto::tonic::trace::v1::Span>,
    ) {
        self.db()
            .await_span_hops(trace_id, expected_sris, max_attempts)
            .await
    }

    /// Read the latest exported cell command/event counters, from whichever
    /// backend this swarm exports telemetry to (per-host files, or the
    /// telemetry DB).
    pub async fn cell_interaction_metrics(&self) -> CellInteractionMetricsSnapshot {
        if let Some(files) = self.process.telemetry_files() {
            return CellInteractionMetricsSnapshot::from_metrics(&files.latest_metrics().await);
        }
        self.db().cell_interaction_metrics().await
    }

    /// Read the latest exported replication wire counters, summed across every
    /// node — see [`crate::metrics::ReplicationMetrics`]. Only available from
    /// the per-host telemetry files; these carry no `sri`, so the telemetry-DB
    /// path (which keys everything by cell) has nothing to read them from.
    pub async fn replication_metrics(&self) -> crate::metrics::ReplicationMetrics {
        match self.process.telemetry_files() {
            Some(files) => {
                crate::metrics::ReplicationMetrics::from_metrics(&files.latest_metrics().await)
            }
            None => crate::metrics::ReplicationMetrics::default(),
        }
    }

    /// Ground-truth count of commands still waiting to be processed, summed across every
    /// deployed cell — see [`crate::clients::db::DbHandle::command_backlog`].
    pub async fn command_backlog(&self) -> usize {
        self.db().command_backlog().await
    }

    /// Ground-truth command backlog per deployed cell — see
    /// [`crate::clients::db::DbHandle::cell_db_state`].
    pub async fn cell_db_state(&self) -> Vec<crate::clients::db::CellDbState> {
        self.db().cell_db_state().await
    }

    /// Ground-truth event count per event name/topic — see
    /// [`crate::clients::db::DbHandle::event_topic_state`]. The counts are
    /// always live db reads; only the event-name discovery follows the
    /// telemetry backend.
    pub async fn event_topic_state(&self) -> Vec<crate::clients::db::EventTopicState> {
        if let Some(files) = self.process.telemetry_files() {
            let names = crate::metrics::event_names(&files.latest_metrics().await);
            return self.db().event_topic_state_for(names).await;
        }
        self.db().event_topic_state().await
    }

    /// Force all telemetry providers in the spawned swarm process to flush.
    pub async fn force_flush_telemetry(&self) {
        self.process.force_flush_telemetry().await;
    }

    /// Waits for a load pass to actually finish, in two phases, then returns the final metrics
    /// snapshot and a [`Completeness`] saying whether it got there — completely, with nothing
    /// missing — before `max_wait`, and if not, why: still climbing when time ran out (a genuine
    /// backlog, needs a longer `max_wait`) vs. detected as permanently stuck partway (see below —
    /// no `max_wait`, however long, would have helped, so this doesn't wait one out).
    ///
    /// Useful after driving load into the swarm: raw counters or trace spans captured before
    /// processing has actually finished just reflect how far along it happened to be at an
    /// arbitrary moment, not whether it completed — this waits for the real signal instead.
    ///
    /// # Why two phases, and why both use a target rather than a stability check
    ///
    /// A pure "stop changing for N samples" check (what phase 1 used to do, over
    /// `commands_received`) cannot tell "genuinely finished" apart from "silently stuck forever"
    /// — both look identical: nothing changes. That distinction matters in practice, not just in
    /// theory: the db layer's cursor-based mailbox polling (`Cursor::After(last_seen_id)`) assumes
    /// row ids are monotonic in commit order, but ids are derived from each transaction's
    /// *begin*-time timestamp — under contention, a transaction that begins earlier can commit
    /// later than one that began after it, landing a row behind a cursor that has already
    /// advanced past it. That row becomes permanently invisible to every future poll on that
    /// cursor, with no error anywhere. A stability check mistakes this for success; nothing short
    /// of an independent, known target can tell the difference.
    ///
    /// 1. Poll [`Self::command_backlog`] — a live `tb_count` over every deployed cell's command
    ///    table, not a cursor-derived metric — until it reaches **zero**. This is a real target,
    ///    not an inferred one: a mailbox row is only ever deleted by the specific poll that
    ///    consumes it (see `cell-mailbox`'s `CommandStream::poll_batch`), and nothing else in this
    ///    codebase ever removes one — no independent TTL/GC touches this table. So a row skipped
    ///    by the cursor race above was never polled, hence never deleted, and stays counted here
    ///    forever: zero really does mean "nothing left," and a stuck nonzero count really does
    ///    mean "something is permanently stuck," not "still draining slowly." Unlike the old
    ///    stability check, this needs no scenario-specific assumption about how much load was
    ///    dispatched or by whom — it reads the swarm's own cell registry, so it covers cells that
    ///    generate load internally (e.g. a timer-driven producer) just as well as ones driven by
    ///    an externally-dispatched call.
    /// 2. Snapshot `events_sent` at that point as a fixed target (once commands are fully drained,
    ///    no cell will process another command, so no cell will publish another event either —
    ///    the current `events_sent` total *is* the target, known exactly), then poll until
    ///    `events_received` actually *reaches* it (not merely stops changing) — but if it goes
    ///    `stable_rounds` samples without any progress at all towards that target, give up right
    ///    there instead of continuing to wait out `max_wait`. The row-visibility bug above is
    ///    permanent, not transient: a poll that would eventually see a lost row's downstream event
    ///    is never coming, no matter how many more times it's tried, so there is nothing to be
    ///    gained by waiting longer once progress has genuinely stopped short of the target —
    ///    `max_wait` only needs to be long enough to ride out a real backlog still draining
    ///    (progress continuing, just slowly), not a stall.
    ///
    /// This assumes events are only ever published from a command handler, never from an event
    /// handler — i.e. no event-triggers-event chains. That holds for every scenario this harness
    /// currently drives (commands flow inward, a terminal command handler publishes an event, the
    /// event is a leaf), but isn't universally true; a scenario that publishes an event *from* an
    /// event handler would need a third phase (or a differently-targeted second one), since events
    /// stopping wouldn't be implied by commands stopping.
    ///
    /// `max_wait` bounds the two phases together, not each separately.
    ///
    /// `poll_interval` is deliberately coarse, not a tight loop: each `events_received` sample in
    /// phase 2 calls [`Self::force_flush_telemetry`], which has a real cost, so this trades off
    /// latency in detecting completion for not hammering the system with flushes while it's still
    /// busy. [`Self::force_flush_telemetry`] also blocks until the trace exporter has durably
    /// written every span created so far, so once this returns, querying spans for any trace
    /// dispatched before the call needs no further retrying — everything that will ever exist
    /// already does. Phase 1's `tb_count` reads need no flush — they're always live.
    pub async fn wait_for_completeness(
        &self,
        poll_interval: Duration,
        stable_rounds: u32,
        max_wait: Duration,
    ) -> (CellInteractionMetricsSnapshot, Completeness) {
        let deadline = std::time::Instant::now() + max_wait;

        let (_, drained) = self
            .poll_until(
                poll_interval,
                stable_rounds,
                deadline,
                async || self.command_backlog().await,
                |backlog| *backlog == 0,
            )
            .await;
        if drained != Completeness::Complete {
            self.force_flush_telemetry().await;
            return (self.cell_interaction_metrics().await, drained);
        }

        self.force_flush_telemetry().await;
        let metrics = self.cell_interaction_metrics().await;
        let target = metrics.totals().events_sent;
        self.poll_until(
            poll_interval,
            stable_rounds,
            deadline,
            async || {
                self.force_flush_telemetry().await;
                self.cell_interaction_metrics().await
            },
            |metrics| metrics.totals().events_received >= target,
        )
        .await
    }

    /// Polls `sample` until `done(&sample)` is true, or gives up — either because the sampled
    /// value has gone `stable_rounds` consecutive rounds unchanged without satisfying `done`
    /// (permanent — see [`Self::wait_for_completeness`]), or because `deadline` passed while it
    /// was still changing (a genuine backlog that just needed more time).
    async fn poll_until<T: PartialEq + Clone>(
        &self,
        poll_interval: Duration,
        stable_rounds: u32,
        deadline: std::time::Instant,
        sample: impl AsyncFn() -> T,
        done: impl Fn(&T) -> bool,
    ) -> (T, Completeness) {
        let mut previous = None;
        let mut stalled_count = 0;

        loop {
            let current = sample().await;
            if done(&current) {
                return (current, Completeness::Complete);
            }

            if previous.as_ref() == Some(&current) {
                stalled_count += 1;
                if stalled_count >= stable_rounds {
                    return (current, Completeness::Stalled);
                }
            } else {
                stalled_count = 0;
            }
            previous = Some(current.clone());

            let now = std::time::Instant::now();
            if now >= deadline {
                return (current, Completeness::TimedOut);
            }
            tokio::time::sleep(poll_interval.min(deadline - now)).await;
        }
    }

    /// Read a cell's persisted state (stored under `key`) and assert it matches `expected`.
    pub async fn assert_cell_state<S>(&self, sri: &str, key: &str, expected: S)
    where
        S: DeserializeOwned + Debug + PartialEq,
    {
        let state = self
            .get_cell_state::<S>(sri, key)
            .await
            .expect("state should be present");
        assert_eq!(expected, state);
    }

    /// Send a fire-and-forget command and wait for the cell's reply on `reply_event`.
    ///
    /// Commands are fire-and-forget: a cell answers by publishing on an event the host observes.
    /// This subscribes to `reply_event` before sending (so the reply cannot be missed), sends the
    /// command, and returns the payload of the next event received. Panics if no reply arrives.
    pub async fn command_await_event(
        &mut self,
        sri: &str,
        cmd_name: &str,
        payload: Option<Vec<u8>>,
        reply_event: &str,
    ) -> Vec<u8> {
        let mut replies = self.subscribe_cell_event(reply_event).await;
        self.command_send(sri, cmd_name, payload).await;

        replies
            .receive()
            .await
            .expect("failed to receive reply event")
    }
}
