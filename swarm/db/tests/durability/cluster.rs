//! Orchestrator side: spawns node workers as child processes, brokers
//! `ReplicaMessage`s between them (zenoh-like bus), kills/restarts nodes,
//! and pumps worker events into the oracle.

use crate::oracle::Oracle;
use crate::proto::{self, HeadEntry, NodeId, Op, ToParent, ToWorker, TxId};
use anyhow::Context as _;
use db_commons::models::{ReplicaMessage, Scope};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

/// One replica message routed from one node to another.
#[derive(Clone)]
pub struct RouteEntry {
    pub from: String,
    pub to: String,
    pub kind: &'static str,
}

struct Link {
    node_id: NodeId,
    to_worker: mpsc::UnboundedSender<ToWorker>,
}

#[derive(Debug)]
enum Event {
    Hello {
        name: String,
    },
    Tx {
        id: TxId,
        ts: u64,
        ok: bool,
        error: Option<String>,
    },
    Dump {
        name: String,
        scope: Scope,
        entries: Vec<(String, String)>,
    },
    Heads {
        name: String,
        heads: Vec<HeadEntry>,
    },
    Disconnected {
        name: String,
    },
}

struct Registry {
    links: Mutex<HashMap<String, Link>>,
    route_log: Mutex<Vec<RouteEntry>>,
    last_route: Mutex<Instant>,
    events: mpsc::UnboundedSender<Event>,
}

impl Registry {
    /// Fan a replica message out to every other connected node.
    fn route(&self, from_name: &str, from_id: NodeId, payload: &[u8]) {
        let kind = postcard::from_bytes::<ReplicaMessage>(payload).map_or("INVALID", |m| m.name());

        let links = self.links.lock().expect("links lock");
        let mut log = self.route_log.lock().expect("route log lock");

        for (name, link) in links.iter() {
            if name == from_name {
                continue;
            }
            let sent = link.to_worker.send(ToWorker::Replica {
                from: from_id,
                payload: payload.to_vec(),
            });
            if sent.is_ok() {
                log.push(RouteEntry {
                    from: from_name.to_string(),
                    to: name.clone(),
                    kind,
                });
            }
        }

        *self.last_route.lock().expect("last route lock") = Instant::now();
    }
}

pub struct NodeState {
    pub log: PathBuf,
    child: Option<std::process::Child>,
}

pub struct Cluster {
    pub oracle: Oracle,
    root: PathBuf,
    keep_root: bool,
    sock: PathBuf,
    namespace: String,
    gc_ms: Option<u64>,
    registry: Arc<Registry>,
    events: mpsc::UnboundedReceiver<Event>,
    nodes: BTreeMap<String, NodeState>,
    live: HashSet<String>,
    expected_down: HashSet<String>,
}

impl Cluster {
    pub fn new(scenario: &str, namespace: &str, gc_ms: Option<u64>) -> anyhow::Result<Self> {
        let root =
            std::env::temp_dir().join(format!("db-durability-{scenario}-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root).context("unable to clear scenario dir")?;
        }
        std::fs::create_dir_all(&root).context("unable to create scenario dir")?;

        let sock = root.join("broker.sock");
        let listener = UnixListener::bind(&sock).context("unable to bind broker socket")?;

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let registry = Arc::new(Registry {
            links: Mutex::new(HashMap::new()),
            route_log: Mutex::new(Vec::new()),
            last_route: Mutex::new(Instant::now()),
            events: events_tx,
        });

        tokio::spawn({
            let registry = registry.clone();
            async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    tokio::spawn(handle_connection(stream, registry.clone()));
                }
            }
        });

        Ok(Self {
            oracle: Oracle::default(),
            root,
            keep_root: false,
            sock,
            namespace: namespace.to_string(),
            gc_ms,
            registry,
            events: events_rx,
            nodes: BTreeMap::new(),
            live: HashSet::new(),
            expected_down: HashSet::new(),
        })
    }

    /// Spawn (or respawn) a node worker and wait until it says hello.
    pub async fn spawn(&mut self, name: &str) -> anyhow::Result<()> {
        let dir = self.root.join(name);
        let log = self.root.join(format!("{name}.log"));

        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .context("unable to open worker log")?;

        let exe = std::env::current_exe().context("unable to find own executable")?;
        let mut cmd = std::process::Command::new(exe);
        cmd.env(proto::env::NAME, name)
            .env(proto::env::SOCKET, &self.sock)
            .env(proto::env::DIR, &dir)
            .env(proto::env::NAMESPACE, &self.namespace)
            .env(proto::env::LOG, &log)
            .stdout(log_file.try_clone().context("unable to clone log handle")?)
            .stderr(log_file)
            .stdin(std::process::Stdio::null());
        if let Some(gc_ms) = self.gc_ms {
            cmd.env(proto::env::GC_MS, gc_ms.to_string());
        }

        let child = cmd.spawn().context("unable to spawn worker")?;

        self.nodes.insert(
            name.to_string(),
            NodeState {
                log,
                child: Some(child),
            },
        );
        self.expected_down.remove(name);

        self.pump_until(
            Duration::from_secs(10),
            |event| matches!(event, Event::Hello { name: n, .. } if n == name),
        )
        .await
        .with_context(|| format!("worker {name} never said hello"))?;

        self.live.insert(name.to_string());
        Ok(())
    }

    /// SIGKILL a node, then absorb every ack it managed to send before dying.
    /// Whatever is still unresolved afterwards becomes indeterminate.
    pub async fn kill(&mut self, name: &str) -> anyhow::Result<()> {
        self.expected_down.insert(name.to_string());
        self.live.remove(name);

        let node = self
            .nodes
            .get_mut(name)
            .with_context(|| format!("unknown node {name}"))?;
        let mut child = node.child.take().context("node has no process")?;

        child.kill().context("unable to SIGKILL worker")?;
        child.wait().context("unable to reap worker")?;

        // The reader task drains the socket to EOF before reporting the
        // disconnect, so once we see it, every pre-kill ack is in the oracle.
        self.pump_until(
            Duration::from_secs(10),
            |event| matches!(event, Event::Disconnected { name: n } if n == name),
        )
        .await
        .context("worker disconnect never surfaced")?;

        self.oracle.mark_indeterminate(name);
        Ok(())
    }

    pub fn live_nodes(&self) -> Vec<String> {
        self.live.iter().cloned().collect()
    }

    pub fn is_live(&self, name: &str) -> bool {
        self.live.contains(name)
    }

    fn send(&self, name: &str, msg: ToWorker) -> anyhow::Result<()> {
        let links = self.registry.links.lock().expect("links lock");
        let link = links
            .get(name)
            .with_context(|| format!("node {name} not connected"))?;
        link.to_worker
            .send(msg)
            .map_err(|_| anyhow::anyhow!("node {name} writer is gone"))
    }

    /// Submit a transaction without waiting for its result.
    pub fn submit(
        &mut self,
        name: &str,
        ops: Vec<Op>,
        retention_ms: Option<u64>,
    ) -> anyhow::Result<TxId> {
        let id = self.oracle.begin(name, ops.clone(), retention_ms.is_some());
        self.send(
            name,
            ToWorker::RunTx(proto::TxSpec {
                id,
                ops,
                retention_ms,
            }),
        )?;
        Ok(id)
    }

    pub fn announce_all(&mut self) -> anyhow::Result<()> {
        for name in self.live_nodes() {
            self.send(&name, ToWorker::Announce)?;
        }
        Ok(())
    }

    /// Pump events until the oracle has seen `n` transaction results in total.
    pub async fn await_results(&mut self, n: usize, deadline: Duration) -> anyhow::Result<()> {
        let oracle_done = |oracle: &Oracle| oracle.resolved_count() >= n;
        if oracle_done(&self.oracle) {
            return Ok(());
        }
        // Borrow dance: check the oracle between events instead of in the predicate.
        let start = Instant::now();
        loop {
            let remaining = deadline
                .checked_sub(start.elapsed())
                .context("timed out waiting for tx results")?;
            self.pump_one(remaining).await?;
            if oracle_done(&self.oracle) {
                return Ok(());
            }
        }
    }

    pub async fn dump(
        &mut self,
        name: &str,
        scope: &Scope,
    ) -> anyhow::Result<Vec<(String, String)>> {
        self.send(
            name,
            ToWorker::Dump {
                scope: scope.clone(),
            },
        )?;

        let mut result = None;
        let scope = scope.clone();
        let target = name.to_string();
        self.pump_until_with(Duration::from_secs(30), |event| match event {
            Event::Dump {
                name,
                scope: s,
                entries,
            } if *name == target && *s == scope => {
                result = Some(std::mem::take(entries));
                true
            }
            _ => false,
        })
        .await
        .with_context(|| format!("dump of {target} timed out"))?;

        Ok(result.expect("dump result set by predicate"))
    }

    pub async fn heads(&mut self, name: &str) -> anyhow::Result<Vec<HeadEntry>> {
        self.send(name, ToWorker::Heads)?;

        let mut result = None;
        let target = name.to_string();
        self.pump_until_with(Duration::from_secs(30), |event| match event {
            Event::Heads { name, heads } if *name == target => {
                result = Some(std::mem::take(heads));
                true
            }
            _ => false,
        })
        .await
        .with_context(|| format!("heads of {target} timed out"))?;

        let mut heads = result.expect("heads result set by predicate");
        heads.sort_by_key(HeadEntry::sort_key);
        Ok(heads)
    }

    /// Wait until the broker has routed at least `n` messages of `kind` to `name`.
    pub async fn await_routed_to(
        &mut self,
        name: &str,
        kind: &str,
        n: usize,
        deadline: Duration,
    ) -> anyhow::Result<()> {
        let start = Instant::now();
        loop {
            let count = {
                let log = self.registry.route_log.lock().expect("route log lock");
                log.iter()
                    .filter(|e| e.to == name && e.kind == kind)
                    .count()
            };
            if count >= n {
                return Ok(());
            }
            anyhow::ensure!(
                start.elapsed() < deadline,
                "timed out waiting for {n} {kind} routed to {name} (saw {count})"
            );
            // Keep absorbing acks while we watch the route log.
            let _ = self.pump_one(Duration::from_millis(20)).await;
        }
    }

    /// Absorb worker events (acks etc.) for a fixed duration.
    pub async fn pump_for(&mut self, duration: Duration) -> anyhow::Result<()> {
        let start = Instant::now();
        while let Some(remaining) = duration.checked_sub(start.elapsed()) {
            self.pump_one(remaining).await?;
        }
        Ok(())
    }

    /// Wait until no replica traffic has been routed for `idle`.
    pub async fn quiesce(&mut self, idle: Duration, deadline: Duration) -> anyhow::Result<()> {
        let start = Instant::now();
        loop {
            let last = *self.registry.last_route.lock().expect("last route lock");
            if last.elapsed() >= idle {
                return Ok(());
            }
            anyhow::ensure!(
                start.elapsed() < deadline,
                "replica traffic never quiesced within {deadline:?}"
            );
            let _ = self.pump_one(Duration::from_millis(50)).await;
        }
    }

    /// Drive announce rounds until every live node reports identical dumps and
    /// heads for the given scopes (ignoring keys under `ignore_prefix`).
    pub async fn settle(
        &mut self,
        scopes: &[Scope],
        ignore_prefix: Option<&str>,
        deadline: Duration,
    ) -> anyhow::Result<ClusterState> {
        let start = Instant::now();
        let mut last_divergence = String::new();

        while start.elapsed() < deadline {
            self.announce_all()?;
            self.quiesce(Duration::from_millis(500), Duration::from_mins(1))
                .await?;

            let state = self.collect(scopes).await?;
            match state.divergence(ignore_prefix) {
                None => return Ok(state),
                Some(divergence) => last_divergence = divergence,
            }
        }

        anyhow::bail!("cluster failed to converge within {deadline:?}: {last_divergence}")
    }

    /// Dump + heads of every live node.
    pub async fn collect(&mut self, scopes: &[Scope]) -> anyhow::Result<ClusterState> {
        let mut state = ClusterState::default();
        for name in self.live_nodes() {
            let mut scoped = BTreeMap::new();
            for scope in scopes {
                let entries = self.dump(&name, scope).await?;
                scoped.insert(
                    scope_key(scope),
                    entries.into_iter().collect::<BTreeMap<_, _>>(),
                );
            }
            let heads = self.heads(&name).await?;
            state.dumps.insert(name.clone(), scoped);
            state.heads.insert(name.clone(), heads);
        }
        Ok(state)
    }

    /// Pump a single event (with timeout), updating oracle and liveness state.
    async fn pump_one(&mut self, timeout: Duration) -> anyhow::Result<bool> {
        let event = tokio::time::timeout(timeout, self.events.recv()).await;
        let Ok(event) = event else {
            return Ok(false); // timeout — not an error here
        };
        let event = event.context("event channel closed")?;
        self.absorb(event)?;
        Ok(true)
    }

    async fn pump_until(
        &mut self,
        deadline: Duration,
        mut predicate: impl FnMut(&Event) -> bool,
    ) -> anyhow::Result<()> {
        self.pump_until_with(deadline, move |event| predicate(event))
            .await
    }

    /// Like `pump_until`, but the predicate may steal data out of the event.
    async fn pump_until_with(
        &mut self,
        deadline: Duration,
        mut predicate: impl FnMut(&mut Event) -> bool,
    ) -> anyhow::Result<()> {
        let start = Instant::now();
        loop {
            let remaining = deadline
                .checked_sub(start.elapsed())
                .context("timed out waiting for worker event")?;
            let event = tokio::time::timeout(remaining, self.events.recv())
                .await
                .ok()
                .flatten()
                .context("timed out waiting for worker event")?;

            let mut event = event;
            let hit = predicate(&mut event);
            self.absorb(event)?;
            if hit {
                return Ok(());
            }
        }
    }

    fn absorb(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Tx { id, ts, ok, error } => {
                self.oracle.on_result(id, ts, ok, error.as_deref());
            }
            Event::Hello { .. } | Event::Dump { .. } | Event::Heads { .. } => {}
            Event::Disconnected { name } => {
                anyhow::ensure!(
                    self.expected_down.contains(&name),
                    "worker {name} died unexpectedly — check {}",
                    self.nodes
                        .get(&name)
                        .map(|n| n.log.display().to_string())
                        .unwrap_or_default(),
                );
            }
        }
        Ok(())
    }

    /// Keep data dirs and logs around for forensics.
    pub fn keep_artifacts(&mut self) -> PathBuf {
        self.keep_root = true;
        self.root.clone()
    }

    pub fn route_log_tail(&self, n: usize) -> Vec<RouteEntry> {
        let log = self.registry.route_log.lock().expect("route log lock");
        log.iter().rev().take(n).rev().cloned().collect()
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        for node in self.nodes.values_mut() {
            if let Some(child) = node.child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if !self.keep_root {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

pub fn scope_key(scope: &Scope) -> String {
    format!("{}/{}/{}", scope.namespace, scope.database, scope.schema)
}

/// Snapshot of every live node's visible state.
#[derive(Default, Debug)]
pub struct ClusterState {
    /// node → scope → key → value
    pub dumps: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,
    /// node → sorted frontier
    pub heads: BTreeMap<String, Vec<HeadEntry>>,
}

impl ClusterState {
    /// Returns a human-readable description of the first divergence between
    /// nodes, or `None` if all nodes agree.
    pub fn divergence(&self, ignore_prefix: Option<&str>) -> Option<String> {
        let mut nodes = self.dumps.iter();
        let (first_name, first_dump) = nodes.next()?;

        let filter = |dump: &BTreeMap<String, BTreeMap<String, String>>| {
            let mut filtered = dump.clone();
            if let Some(prefix) = ignore_prefix {
                for scoped in filtered.values_mut() {
                    scoped.retain(|key, _| !key.starts_with(prefix));
                }
            }
            filtered
        };

        let reference = filter(first_dump);
        for (name, dump) in nodes {
            let dump = filter(dump);
            if dump != reference {
                return Some(describe_dump_diff(first_name, &reference, name, &dump));
            }
        }

        let mut heads = self.heads.iter();
        let (first_name, first_heads) = heads.next()?;
        for (name, node_heads) in heads {
            if node_heads != first_heads {
                return Some(format!(
                    "frontier divergence: {first_name} has {} heads, {name} has {} heads",
                    first_heads.len(),
                    node_heads.len(),
                ));
            }
        }

        None
    }
}

fn describe_dump_diff(
    a_name: &str,
    a: &BTreeMap<String, BTreeMap<String, String>>,
    b_name: &str,
    b: &BTreeMap<String, BTreeMap<String, String>>,
) -> String {
    for (scope, a_entries) in a {
        let empty = BTreeMap::new();
        let b_entries = b.get(scope).unwrap_or(&empty);
        for (key, a_value) in a_entries {
            match b_entries.get(key) {
                Some(b_value) if b_value == a_value => {}
                Some(b_value) => {
                    return format!(
                        "{scope}:{key} differs: {a_name}={a_value:?} {b_name}={b_value:?}"
                    );
                }
                None => {
                    return format!("{scope}:{key} on {a_name} ({a_value:?}) missing on {b_name}");
                }
            }
        }
        for key in b_entries.keys() {
            if !a_entries.contains_key(key) {
                return format!("{scope}:{key} on {b_name} missing on {a_name}");
            }
        }
    }
    format!("{a_name} and {b_name} disagree on scope sets")
}

async fn handle_connection(stream: UnixStream, registry: Arc<Registry>) {
    let (mut read_half, mut write_half) = stream.into_split();

    let hello: ToParent = match proto::read_frame(&mut read_half).await {
        Ok(frame) => frame,
        Err(_) => return,
    };
    let ToParent::Hello { name, node_id, .. } = hello else {
        return;
    };

    let (to_worker, mut to_worker_rx) = mpsc::unbounded_channel::<ToWorker>();
    tokio::spawn(async move {
        while let Some(frame) = to_worker_rx.recv().await {
            if proto::write_frame(&mut write_half, &frame).await.is_err() {
                break; // worker died; reader side reports it
            }
        }
    });

    registry
        .links
        .lock()
        .expect("links lock")
        .insert(name.clone(), Link { node_id, to_worker });
    let _ = registry.events.send(Event::Hello { name: name.clone() });

    loop {
        match proto::read_frame::<ToParent, _>(&mut read_half).await {
            Ok(ToParent::Replica { payload }) => registry.route(&name, node_id, &payload),
            Ok(ToParent::TxResult { id, ts, ok, error }) => {
                let _ = registry.events.send(Event::Tx { id, ts, ok, error });
            }
            Ok(ToParent::DumpResult { scope, entries }) => {
                let _ = registry.events.send(Event::Dump {
                    name: name.clone(),
                    scope,
                    entries,
                });
            }
            Ok(ToParent::HeadsResult { heads }) => {
                let _ = registry.events.send(Event::Heads {
                    name: name.clone(),
                    heads,
                });
            }
            // No graceful-shutdown flow in the orchestrator; kills only.
            Ok(ToParent::Done) => {}
            Ok(ToParent::Hello { .. }) => break, // protocol violation
            Err(_) => break,                     // EOF — killed or exited
        }
    }

    // Only remove the link if it is still ours: a restarted worker may have
    // already replaced it.
    {
        let mut links = registry.links.lock().expect("links lock");
        if links.get(&name).is_some_and(|link| link.node_id == node_id) {
            links.remove(&name);
        }
    }
    let _ = registry.events.send(Event::Disconnected { name });
}
