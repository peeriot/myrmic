use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};
use zenoh::Session;

use cell_protocol::node_tags::LiveTags;
use swarm_api::DropNotifier;

use crate::config::PluginConfigs;
pub use config::SwarmConfig;
#[cfg(feature = "plugin-orchestration")]
pub use plugins::orchestration::Config as OrchestrationConfig;
pub use test_hooks::TestHooks;

/// How long a swarm's plugins may take to report ready once its zenoh runtime started.
pub const PLUGIN_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

pub mod spawn;

mod config;
mod input;
#[cfg(any(feature = "plugin-db", feature = "plugin-execution"))]
mod node_tags;
mod plugins;
mod test_hooks;

pub struct Swarm {
    config: SwarmConfig,
}

impl FromStr for Swarm {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Swarm {
    pub fn parse(content: impl AsRef<str>) -> anyhow::Result<Self> {
        let config = input::eval_str::<SwarmConfig, _>(content)?;
        Ok(Self::new(config))
    }

    pub fn from_path(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let config = eval_input::<SwarmConfig>(path)?;
        Ok(Self::new(config))
    }

    pub fn new(config: SwarmConfig) -> Self {
        Self { config }
    }

    pub fn into_config(self) -> SwarmConfig {
        self.config
    }

    /// Creates the session on the current tokio runtime, and waits for it to fully start.
    /// Once this function returns, the session has been started (plugins included)
    pub async fn wait_in_place(self) -> anyhow::Result<spawn::Spawned> {
        self.spawn_in_place()?.wait().await
    }

    /// Starts the configured session on the current runtime (errors if no runtime found).
    /// Returns a `Spawning` that can be awaited to get the fully started `Spawned`.
    pub fn spawn_in_place(self) -> anyhow::Result<spawn::Spawning> {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            anyhow::bail!("Not running on a tokio runtime")
        };

        let (drop_tx, drop_rx) = flume::bounded(1);
        let fut = spawn(self.config, drop_rx);

        Ok(spawn::Spawning {
            handle: handle.spawn(fut),
            kill_signal: drop_tx,
        })
    }
}

#[allow(clippy::too_many_lines)]
#[tracing::instrument(skip_all, fields(mode = %config.zenoh.mode().unwrap_or_default(), id = ?config.zenoh.id()))]
async fn spawn(config: SwarmConfig, drop_rx: DropNotifier) -> anyhow::Result<spawn::SwarmSession> {
    tracing::info!("Creating session");

    let SwarmConfig {
        mut zenoh,
        telemetry,
        plugins,
    } = config;

    zenoh
        .insert_json5("timestamping/enabled", "true")
        .expect("valid timestamping config");

    let mut runtime = zenoh::internal::runtime::RuntimeBuilder::new(zenoh)
        .build()
        .await
        .expect("Failed to create runtime");

    let dyn_runtime = zenoh::internal::runtime::DynamicRuntime::from(runtime.clone());

    let session = zenoh::session::init(dyn_runtime.clone())
        .await
        .expect("Unable to initialise session");

    let handle = tokio::runtime::Handle::current();

    let log_dir = telemetry.logs.directory.clone();
    let telemetry_guard = if swarm_telemetry::TelemetryConfig::has_global() {
        record_telemetry_install_outcome(
            log_dir.as_deref(),
            "skipped: a global tracing subscriber was already set",
        );
        None
    } else {
        let result = telemetry
            .try_global("swarm".into(), &session)
            .inspect_err(|err| tracing::warn!("failed to install telemetry: {err}"));
        let outcome = match &result {
            Ok(_) => "ok".to_owned(),
            Err(err) => format!("failed: {err}"),
        };
        record_telemetry_install_outcome(log_dir.as_deref(), &outcome);
        result.ok().map(Arc::new)
    };
    let telemetry_control_handle = telemetry_guard
        .as_ref()
        .map(|guard| guard.clone().force_flush_queryable(&session));

    let plugins = Arc::new(plugins);

    // Started before any plugin, so a plugin reading tags on its first pass
    // sees the node's real set rather than the configured one.
    #[cfg(any(feature = "plugin-db", feature = "plugin-execution"))]
    let tags = {
        let configured = node_tags::configured(&plugins);
        let tags = LiveTags::new(node_tags::effective_at_boot(&session, &configured));

        handle.spawn(node_tags::watch(session.clone(), tags.clone(), configured));

        tags
    };
    #[cfg(not(any(feature = "plugin-db", feature = "plugin-execution")))]
    let tags = LiveTags::default();

    macro_rules! load_plugin {
        ($started:ident, $feature:literal, $plugin:ty, |$conf:ident| $configured_body:expr $(,)?) => {
            #[cfg(feature = $feature)]
            {
                let init = init_plugin::<$plugin>(
                    plugins.clone(),
                    &session,
                    &tags,
                    &drop_rx,
                    &handle,
                    |$conf| $configured_body,
                );
                if let Some((task, ready)) = init {
                    $started.push((
                        <$plugin as plugins::MyrmicPlugin>::DEFAULT_NAME,
                        ready,
                        task,
                    ));
                }
            }
        };
    }

    #[cfg_attr(not(feature = "plugin-db"), allow(unused_mut))]
    let mut first = vec![];
    load_plugin!(first, "plugin-db", plugins::db::Plugin, |p| p.db.clone());

    runtime.start().await.expect("Failed to start runtime");

    let rest = || {
        let mut started = vec![];
        load_plugin!(started, "plugin-mqtt", plugins::mqtt::Plugin, |p| p
            .mqtt
            .clone());
        load_plugin!(
            started,
            "plugin-orchestration",
            plugins::orchestration::SorgOrchestrationPlugin,
            |p| p.orchestration.clone(),
        );
        load_plugin!(
            started,
            "plugin-execution",
            plugins::execution::SorgExecutionPlugin,
            |p| p.execution.clone()
        );
        load_plugin!(
            started,
            "plugin-gateway",
            plugins::gateway::Plugin,
            |plugins| { plugins.gateway.clone() }
        );
        load_plugin!(
            started,
            "plugin-introspection",
            plugins::introspection::IntrospectionPlugin,
            |p| Some(p.introspection.clone()),
        );
        load_plugin!(
            started,
            "plugin-onboarding",
            plugins::onboarding::SwarmOnboardingPlugin,
            |p| p.onboarding.clone(),
        );
        load_plugin!(
            started,
            "plugin-embedded-log",
            plugins::embedded_log::EmbeddedLoggingPlugin,
            |p| p.embedded_log.clone(),
        );

        started
    };

    let started = start_in_order(PLUGIN_STARTUP_TIMEOUT, first, rest).await?;

    let plugin_tasks = started
        .into_iter()
        .map(|(name, _, task)| (name, task))
        .collect();

    #[cfg(feature = "plugin-db")]
    if telemetry_guard.is_some() {
        self_register_tele_replication(&session).await;
    }

    tracing::info!("Session created");

    Ok(spawn::SwarmSession::new(
        session,
        runtime,
        telemetry_guard,
        telemetry_control_handle,
        plugin_tasks,
    ))
}

/// Best-effort: registers this process as the replication holder of its own locally-written
/// `tele` telemetry data.
///
/// A `Constraint::Routed` transaction (used for every telemetry DB read/write — see
/// `db_client::v1::Client::{read,write}_tx_routed`) that names a scope nobody has explicitly
/// claimed doesn't fail, but doesn't durably commit either: `swarm/plugins/db/handler.rs`'s
/// handling of `Constraint::Routed` re-elects the landing node as a *provisional* offloader on
/// every single such transaction, rather than treating it as an established holder — which
/// silently drops the data from any later, independently-routed read. A single local process hits
/// this on every single telemetry write, since nothing else here ever claims the scope. A
/// deployment that runs `myrmic replicate scope:tele -t <tag>` after every host starts overwrites
/// this with one cluster-wide holder — this is just each node's own sane default until (if ever)
/// something says otherwise, and it's what makes the very first telemetry write actually stick
/// instead of racing its own provisional-election window.
#[cfg(feature = "plugin-db")]
async fn self_register_tele_replication(session: &Session) {
    use cell_protocol::replication::{
        REPLICATION_TABLE, ReplicaEntry, ReplicaSelector, replication_scope, runtime_tag,
    };
    use db_commons::models::Subject;

    let client = db_client::v1::Client::new(session);
    let selector = ReplicaSelector::Subject(Subject::Namespace(String::from("tele")));
    let entry = ReplicaEntry::new(
        selector.clone(),
        vec![runtime_tag(client.zid().into())],
        &selector.to_string(),
    );
    let key = entry.key();

    let result = client
        .write_tx(async move |c, tx_id| {
            let value =
                postcard::to_allocvec(&entry).expect("a replication entry should always serialise");
            c.send(db_client::v1::models::tb_insert::Request {
                id: tx_id,
                op: db_client::v1::models::tb_insert::Op {
                    scope: replication_scope(),
                    table: String::from(REPLICATION_TABLE),
                    eid: Some(key.into_bytes()),
                    value,
                },
            })
            .await?
            .map_err(|err| anyhow::anyhow!("{}", err.message))?;
            Ok(())
        })
        .await;

    if let Err(err) = result {
        tracing::warn!("failed to self-register tele replication holder: {err}");
    }
}

/// Best-effort record of whether this process's global telemetry install
/// succeeded. The one diagnostic that cannot go through `tracing`: a failed
/// install is precisely the case where no subscriber exists to carry it, and a
/// daemonized `myrmic runtimes start -d` redirects stdout/stderr to `/dev/null`
/// (see the `fork` crate's `redirect_stdio`), so there is no terminal either.
///
/// Written into the runtime's own configured log directory, alongside the
/// rolling logs `m rt <name> logs` reads — never a fixed path in a world-shared
/// `/tmp`, which another user can pre-create as a symlink to somewhere this
/// process would then append to. With no log directory configured there is
/// nowhere private to put it, and stderr is the best that can be done.
///
/// Never panics — diagnostics must not be able to take down the process they
/// are diagnosing.
fn record_telemetry_install_outcome(log_dir: Option<&std::path::Path>, outcome: &str) {
    use std::io::Write as _;

    let line = format!("pid={} telemetry install {outcome}", std::process::id());

    let Some(dir) = log_dir else {
        eprintln!("{line}");
        return;
    };

    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("telemetry-install.log"));

    match opened {
        Ok(mut file) => {
            let _ = writeln!(file, "{line}");
        }
        Err(err) => eprintln!("{line} (unable to record it in {}: {err})", dir.display()),
    }
}

fn init_plugin<T>(
    plugins: Arc<PluginConfigs>,
    session: &Session,
    tags: &LiveTags,
    drop_rx: &DropNotifier,
    handle: &tokio::runtime::Handle,
    extractor: for<'a> fn(&'a PluginConfigs) -> Option<T::Config>,
) -> Option<(JoinHandle<zenoh::Result<()>>, swarm_api::Ready)>
where
    T: crate::plugins::MyrmicPlugin,
{
    tracing::debug!("init {}", <T as plugins::MyrmicPlugin>::DEFAULT_NAME);

    let Some(conf) = extractor(plugins.as_ref()) else {
        tracing::debug!("skipping {}", <T as plugins::MyrmicPlugin>::DEFAULT_NAME);
        return None;
    };

    tracing::debug!("loading {}", <T as plugins::MyrmicPlugin>::DEFAULT_NAME);

    let ready = swarm_api::Ready::default();

    let ctx = plugins::MyrmicCtx::new(
        session.clone(),
        handle.clone(),
        plugins,
        tags.clone(),
        drop_rx.clone(),
        ready.clone(),
    );

    let fut = <T as plugins::MyrmicPlugin>::main(ctx, conf);

    Some((handle.spawn(fut), ready))
}

type Started = (
    &'static str,
    swarm_api::Ready,
    JoinHandle<zenoh::Result<()>>,
);

/// Waits until every plugin in `started` is ready; fails as soon as one fails or `deadline`
/// passes. A timeout names the plugins of this phase.
async fn all_ready(
    deadline: Instant,
    budget: Duration,
    started: &mut [Started],
) -> anyhow::Result<()> {
    let names = started
        .iter()
        .map(|(name, ..)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    let ready = futures_util::future::try_join_all(
        started
            .iter_mut()
            .map(|(name, ready, task)| plugin_ready(name, ready.clone(), task)),
    );

    timeout_at(deadline, ready)
        .await
        .with_context(|| {
            format!("startup timed out waiting for [{names}] [took longer than {budget:?}]")
        })?
        .map(drop)
}

/// Waits for `first` to be ready, then spawns the plugins that depend on it and waits for those.
/// Both phases share `budget`. The result lists `first` before the rest.
async fn start_in_order<F>(
    budget: Duration,
    mut first: Vec<Started>,
    rest: F,
) -> anyhow::Result<Vec<Started>>
where
    F: FnOnce() -> Vec<Started>,
{
    let deadline = Instant::now() + budget;
    all_ready(deadline, budget, &mut first).await?;
    let mut rest = rest();
    all_ready(deadline, budget, &mut rest).await?;
    first.append(&mut rest);

    Ok(first)
}

/// Resolves once the plugin signalled ready; fails if its task ends first.
async fn plugin_ready(
    name: &str,
    ready: swarm_api::Ready,
    task: &mut JoinHandle<zenoh::Result<()>>,
) -> anyhow::Result<()> {
    tokio::select! {
        // A stored permit wins: a plugin that got ready and then ended is
        // reported by `Spawned::plugin_failure`, not as a startup failure.
        biased;
        () = ready.notified() => Ok(()),
        result = task => match result {
            Ok(Ok(())) => Err(anyhow::anyhow!("plugin {name} stopped before it was ready")),
            Ok(Err(err)) => Err(anyhow::anyhow!("plugin {name} failed to start: {err}")),
            Err(err) => Err(anyhow::anyhow!("plugin {name} failed to start: {err}")),
        },
    }
}

#[doc(hidden)]
pub fn eval_input<T>(path: impl AsRef<Path>) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
{
    input::eval_file::<T, _>(path)
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use futures_util::FutureExt as _;
    use tokio::time::timeout;

    use super::{Started, plugin_ready, start_in_order};

    #[tokio::test]
    async fn ready_then_ok_is_ready() {
        let ready = swarm_api::Ready::default();
        ready.notify_one();
        let mut handle = task(async { Ok(()) });
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }

        plugin_ready("test", ready, &mut handle)
            .await
            .expect("ready wins over an already finished task");
    }

    #[tokio::test]
    async fn still_running_and_ready_is_ready() {
        let ready = swarm_api::Ready::default();
        let signal = ready.clone();
        let mut handle = task(async move {
            signal.notify_one();
            pending::<()>().await;
            Ok(())
        });

        plugin_ready("test", ready, &mut handle)
            .await
            .expect("plugin signalled ready");
        handle.abort();
    }

    #[tokio::test]
    async fn ok_before_ready_fails() {
        let mut handle = task(async { Ok(()) });

        let err = plugin_ready("test", swarm_api::Ready::default(), &mut handle)
            .await
            .expect_err("task ended before ready");
        assert!(
            err.to_string()
                .contains("plugin test stopped before it was ready"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn err_before_ready_fails() {
        let mut handle = task(async { Err("boom".into()) });

        let err = plugin_ready("test", swarm_api::Ready::default(), &mut handle)
            .await
            .expect_err("task failed before ready");
        assert!(
            err.to_string()
                .contains("plugin test failed to start: boom"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn panic_before_ready_fails() {
        let mut handle = task(async { panic!("boom") });

        let err = plugin_ready("test", swarm_api::Ready::default(), &mut handle)
            .await
            .expect_err("task panicked before ready");
        let message = err.to_string();
        assert!(message.contains("plugin test failed to start"), "{message}");
        assert!(message.contains("boom"), "{message}");
    }

    #[tokio::test]
    async fn dependents_start_after_first_is_ready() {
        let first_ready = swarm_api::Ready::default();
        let first = vec![pending_plugin("first", first_ready.clone())];
        let entered = Arc::new(AtomicBool::new(false));
        let seen = entered.clone();
        let rest = move || {
            seen.store(true, Ordering::SeqCst);
            let ready = swarm_api::Ready::default();
            ready.notify_one();

            vec![pending_plugin("rest", ready)]
        };

        let mut startup = pin!(start_in_order(BUDGET, first, rest));
        assert!((&mut startup).now_or_never().is_none());
        assert!(
            !entered.load(Ordering::SeqCst),
            "dependent started before the db was ready"
        );

        first_ready.notify_one();
        let started = timeout(Duration::from_secs(1), startup)
            .await
            .expect("startup finishes once the first plugin is ready")
            .expect("startup succeeds");
        assert!(entered.load(Ordering::SeqCst));
        assert_eq!(
            started.iter().map(|(name, ..)| *name).collect::<Vec<_>>(),
            ["first", "rest"]
        );
        abort_all(started);
    }

    #[tokio::test]
    async fn no_first_plugin_starts_rest_at_once() {
        let ready = swarm_api::Ready::default();
        ready.notify_one();
        let rest = move || vec![pending_plugin("rest", ready)];

        let started = timeout(Duration::from_secs(1), start_in_order(BUDGET, vec![], rest))
            .await
            .expect("startup does not wait for a first plugin")
            .expect("startup succeeds");
        assert_eq!(started.len(), 1);
        abort_all(started);
    }

    #[tokio::test]
    async fn failing_first_plugin_stops_startup() {
        let first = vec![(
            "first",
            swarm_api::Ready::default(),
            task(async { Err("boom".into()) }),
        )];
        let entered = Arc::new(AtomicBool::new(false));
        let seen = entered.clone();
        let rest = move || {
            seen.store(true, Ordering::SeqCst);
            Vec::new()
        };

        let err = start_in_order(BUDGET, first, rest)
            .await
            .expect_err("startup fails with the first plugin");
        assert!(
            err.to_string()
                .contains("plugin first failed to start: boom"),
            "{err}"
        );
        assert!(!entered.load(Ordering::SeqCst), "dependents were started");
    }

    #[tokio::test]
    async fn hanging_first_plugin_is_named_in_the_timeout() {
        let first = vec![pending_plugin("first", swarm_api::Ready::default())];

        let err = start_in_order(Duration::from_millis(50), first, Vec::new)
            .await
            .expect_err("startup times out");
        assert!(
            err.to_string()
                .contains("startup timed out waiting for [first]"),
            "{err}"
        );
    }

    const BUDGET: Duration = Duration::from_secs(30);

    fn pending_plugin(name: &'static str, ready: swarm_api::Ready) -> Started {
        let handle = task(async {
            pending::<()>().await;
            Ok(())
        });

        (name, ready, handle)
    }

    fn abort_all(started: Vec<Started>) {
        for (_, _, handle) in started {
            handle.abort();
        }
    }

    fn task<F>(fut: F) -> tokio::task::JoinHandle<zenoh::Result<()>>
    where
        F: Future<Output = zenoh::Result<()>> + Send + 'static,
    {
        tokio::spawn(fut)
    }
}
