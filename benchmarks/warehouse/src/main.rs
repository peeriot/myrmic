use std::{collections::HashMap, path::PathBuf, time::Duration};

use bench_harness::{
    BenchmarkScenario,
    scenario::{DispatchOutcome, DispatchedCalls},
};
use cell_protocol::replication::ReplicaSelector;
use cell_protocol::{Sri, scope_of_cell};
use rand::Rng;
use serde::{Deserialize, Serialize};
use test_framework::{
    metrics::CellInteractionMetricsSnapshot,
    producers::{LoadConfig, LoadTarget, SelectionStrategy, command::LoadProducer},
    scenario::{SwarmTest, SwarmTestCtx},
};
use warehouse_benchmark::prealloc::{Assignment, preallocate};
use warehouse_benchmark::rack_config::{
    RACK_CENTRAL_TAG, RACK_TELEMETRY_DIR, RackConfig, rack_host_specs, rack_object_tag,
    rack_zone_tag,
};

// copy of the type inside tier_1_object
#[derive(Serialize, Deserialize)]
struct ObjectUpdate {
    bench_id: u64,
    call_id: u64,
    zone_id: u16,
    payload: String,
}

// copy of the type inside input_fixed_camera
#[derive(Serialize, Deserialize)]
struct StartRequest {
    bench_id: u64,
    call_id: u64,
    object_cells: u16,
    zone_cells: u16,
    produce_every_ms: u16,
    payload_size: usize,
    fixed_delay: bool,
}

/// How load is spread across the object and zone cells.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FanOutStrategy {
    /// cycle through every object/zone pair in order
    RoundRobin,
    /// pick an object/zone pair uniformly at random on every send
    Random,
}

impl FanOutStrategy {
    /// Name reported as the `"Fan-out strategy"` run parameter.
    fn as_str(self) -> &'static str {
        match self {
            FanOutStrategy::RoundRobin => "round_robin",
            FanOutStrategy::Random => "random",
        }
    }
}

impl From<FanOutStrategy> for SelectionStrategy {
    fn from(strategy: FanOutStrategy) -> Self {
        match strategy {
            FanOutStrategy::RoundRobin => SelectionStrategy::RoundRobin,
            FanOutStrategy::Random => SelectionStrategy::Random,
        }
    }
}

/// The warehouse benchmark's own config, deserialized from the config file's `[specialized]`
/// table.
#[derive(Deserialize)]
struct Specialized {
    /// number of object twins to deploy.
    num_objects: usize,
    /// number of zone twins to deploy.
    num_zones: usize,
    /// how load is spread across the `num_objects` object cells, and independently, how each
    /// dispatched update's `zone_id` is spread across the `num_zones` zone cells.
    fan_out_strategy: FanOutStrategy,
    /// path to the swarm jsonnet config; defaults to the bundled `local_swarm.jsonnet`.
    #[serde(default)]
    config: Option<PathBuf>,
    /// where load comes from: an external load producer (the benchmark harness itself, the
    /// default) or an internal one (`input_fixed_camera` cells deployed into the swarm).
    #[serde(default)]
    producer: ProducerConfig,
    /// deploy across a rack of remote hosts over SSH instead of spawning one local swarm process
    /// (see [`RackConfig`]). Absent by default — the local/docker path this benchmark has always
    /// used.
    #[serde(default)]
    rack: Option<RackConfig>,
}

/// How load is produced for a pass.
#[derive(Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ProducerKind {
    /// the benchmark harness dispatches every command itself, from outside the swarm.
    #[default]
    External,
    /// `input_fixed_camera` cells, deployed into the swarm, generate load on a timer.
    Internal,
}

/// `[specialized.producer]` — defaults to a single external producer (the harness itself).
#[derive(Deserialize)]
struct ProducerConfig {
    #[serde(default)]
    kind: ProducerKind,
    /// number of `input_fixed_camera` cells to deploy and split the load across, when `kind` is
    /// `internal`. Each replica processes its own timer ticks one at a time (one cell instance
    /// has exactly one message loop), and every tick's `send` does a real registry-lookup +
    /// dispatch round trip before the next tick can even be picked up — in practice this caps a
    /// single replica at a few hundred ticks/sec, well short of the 1000/sec its 1ms timer
    /// granularity would otherwise allow. That ceiling is empirical, not a constant this
    /// benchmark knows exactly — if a load's ingestion percentage (printed per pass) comes in
    /// low, the fix is more replicas, not a smaller `produce_every_ms`.
    #[serde(default = "default_producer_replicas")]
    replicas: usize,
    /// if `true`, each replica's timer waits for the previous tick's `send` to actually finish
    /// before starting the next `produce_every_ms` countdown, rather than firing on a strict
    /// schedule and letting ticks queue up (and eventually block the timer task) once `send`
    /// takes longer than `produce_every_ms` — see `input_fixed_camera`'s `StartRequest` and
    /// `myrmic_sdk::interval`'s `IntervalBuilder::fixed_delay`. Self-throttles to the swarm's
    /// actual per-tick round-trip time instead of measuring how far short of the configured rate
    /// it fell.
    #[serde(default)]
    fixed_delay: bool,
}

impl Default for ProducerConfig {
    fn default() -> Self {
        Self {
            kind: ProducerKind::External,
            replicas: default_producer_replicas(),
            fixed_delay: false,
        }
    }
}

fn default_producer_replicas() -> usize {
    1
}

const ASSET_SRI_TEMPLATE: &str = "asset.object.{n}";
const ZONE_SRI_TEMPLATE: &str = "agent.zone.{n}";
const CENTRAL_SRI: &str = "bridge.central";
const CAMERA_SRI_TEMPLATE: &str = "producer.camera.{n}";

const TIER_1_OBJECT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/cells/tier_1_object");
const TIER_2_ZONE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/cells/tier_2_zone");
const TIER_3_CENTRAL_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/cells/tier_3_central");
const INPUT_FIXED_CAMERA_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/cells/input_fixed_camera");

// TODO: this should be configurable later, for now just 64 ascii chars/bytes
const PAYLOAD_LEN: usize = 64;

/// Above this many ticks/sec, a single internal-producer replica's real per-tick round trip
/// (registry lookup + dispatch, awaited one at a time by its message loop) has been observed to
/// fall well short of its configured timer period — see `dispatch_internal`. Conservative and
/// empirical, not a documented system limit; raise it if a given environment sustains more.
const PRACTICAL_TICK_RATE_WARNING_THRESHOLD: u64 = 400;

/// Derives the SRI of every replica named by substituting `{n}` in `template` for
/// `0..count`. SRIs are UUIDs deterministically derived from the name, so — unlike
/// the name itself — they carry no shared prefix to match on; this enumerates the
/// concrete set instead.
fn sri_range(template: &str, count: usize) -> Vec<Sri> {
    (0..count)
        .map(|n| {
            let name = template.replace("{n}", &n.to_string());
            Sri::of_path(&name).unwrap_or_else(|err| panic!("invalid cell name {name:?}: {err}"))
        })
        .collect()
}

/// Every generated cell's SRI, grouped by tier.
struct Topology {
    asset_sris: Vec<Sri>,
    zone_sris: Vec<Sri>,
    central_sri: Sri,
}

/// The internal producer's cell SRIs, one per configured replica — empty when the producer is
/// external.
fn producer_sris(specialized: &Specialized) -> Vec<Sri> {
    if specialized.producer.kind != ProducerKind::Internal {
        return Vec::new();
    }
    sri_range(CAMERA_SRI_TEMPLATE, specialized.producer.replicas)
}

fn topology(num_objects: usize, num_zones: usize) -> Topology {
    Topology {
        asset_sris: sri_range(ASSET_SRI_TEMPLATE, num_objects),
        zone_sris: sri_range(ZONE_SRI_TEMPLATE, num_zones),
        central_sri: Sri::of_path(CENTRAL_SRI)
            .unwrap_or_else(|err| panic!("invalid central sri: {err}")),
    }
}

/// Dispatches `load` commands/sec for `timeout` seconds from outside the swarm: each send picks
/// an object target according to `fan_out_strategy` over `num_objects`, and independently picks
/// that update's `zone_id` according to the same strategy over `num_zones`.
async fn dispatch_external(
    ctx: &SwarmTestCtx,
    specialized: &Specialized,
    pass_index: usize,
    load: u64,
    timeout: u64,
) -> DispatchOutcome {
    let bench_id = pass_index as u64;
    let call_id = 0;
    let num_zones = u16::try_from(specialized.num_zones).expect("num_zones fits in u16");
    let strategy = specialized.fan_out_strategy;

    let targets = (0..specialized.num_objects)
        .map(|object_id| LoadTarget {
            sri: ASSET_SRI_TEMPLATE.replace("{n}", &object_id.to_string()),
            payload: None,
        })
        .collect();

    let payload_fn: Box<dyn Fn(u64) -> Vec<u8> + Send + Sync> = Box::new(move |n| {
        let zone_id = match strategy {
            FanOutStrategy::RoundRobin => {
                u16::try_from(n % u64::from(num_zones)).expect("n % num_zones fits in u16")
            }
            FanOutStrategy::Random => rand::rng().random_range(0..num_zones),
        };
        let update = ObjectUpdate {
            bench_id,
            call_id,
            zone_id,
            payload: "x".repeat(PAYLOAD_LEN),
        };
        postcard::to_allocvec(&update).expect("failed to serialize ObjectUpdate")
    });

    let load_config = LoadConfig {
        cmd_name: "update".into(),
        targets,
        strategy: specialized.fan_out_strategy.into(),
        rate: load,
        timeout: Duration::from_secs(timeout),
        payload_fn: Some(payload_fn),
    };
    let calls = LoadProducer::new(ctx.sorg_handle(), load_config)
        .produce()
        .await;

    DispatchOutcome {
        calls: DispatchedCalls::Known(calls),
        // each dispatch loop runs for a fixed, deterministic `timeout`, and the per-target
        // rates above sum to exactly `load`, so this is exact
        expected_messages: load.saturating_mul(timeout),
    }
}

/// Drives `load` commands/sec for `timeout` seconds by starting `specialized.producer.replicas`
/// `input_fixed_camera` cells (deployed into the swarm by [`WarehouseScenario::build_ctx`]),
/// splitting the load evenly across them, letting them run for `timeout` seconds, then stopping
/// them.
///
/// Unlike [`dispatch_external`], no call is dispatched from outside the swarm, so there is no
/// externally-known trace id to correlate a call's spans by — this returns
/// [`DispatchedCalls::Discovered`], which has the driver find calls itself by querying spans
/// created during the pass instead (see `bench_harness::driver`'s `discover_calls`).
async fn dispatch_internal(
    ctx: &SwarmTestCtx,
    specialized: &Specialized,
    pass_index: usize,
    load: u64,
    timeout: u64,
) -> DispatchOutcome {
    let replicas = specialized.producer.replicas;
    assert!(replicas > 0, "specialized.producer.replicas must be > 0");

    let per_replica_rate = load / replicas as u64;
    assert!(
        per_replica_rate > 0,
        "load {load}/sec split across {replicas} producer replica(s) rounds down to 0/sec per \
         replica; use fewer replicas or a higher load"
    );
    let produce_every_ms = u16::try_from(1000 / per_replica_rate).unwrap_or_else(|_| {
        panic!(
            "per-replica rate {per_replica_rate}/sec produces a tick interval that doesn't fit \
             in a u16 number of milliseconds"
        )
    });
    // hard ceiling: a timer can't tick faster than once per whole millisecond.
    assert!(
        produce_every_ms > 0,
        "per-replica rate {per_replica_rate}/sec is faster than the internal producer's 1ms \
         tick granularity supports; use more replicas or a lower load"
    );
    // soft ceiling, well below the hard one above: every tick's `send` is a real
    // registry-lookup-then-dispatch round trip, awaited one at a time by the replica's single
    // message loop — in this environment that puts the sustainable rate at only a few hundred
    // ticks/sec per replica. This can't crash on (it's empirical, not a fixed constant), but it's
    // worth flagging up front rather than leaving it to a low ingestion percentage in the report.
    if per_replica_rate > PRACTICAL_TICK_RATE_WARNING_THRESHOLD {
        println!(
            "warning: requesting {per_replica_rate} ticks/sec from a single internal producer \
             replica; each tick's send is a synchronous round trip through that replica's \
             message loop, which in practice tops out well below its {produce_every_ms}ms timer \
             period would allow — expect this pass's ingestion to fall short. Raise \
             specialized.producer.replicas to spread the load thinner."
        );
    }

    let object_cells = u16::try_from(specialized.num_objects).expect("num_objects fits in u16");
    let zone_cells = u16::try_from(specialized.num_zones).expect("num_zones fits in u16");
    let start = StartRequest {
        bench_id: pass_index as u64,
        call_id: 0,
        object_cells,
        zone_cells,
        produce_every_ms,
        payload_size: PAYLOAD_LEN,
        fixed_delay: specialized.producer.fixed_delay,
    };
    let payload = postcard::to_allocvec(&start).expect("failed to serialize StartRequest");

    let camera_names: Vec<String> = (0..replicas)
        .map(|n| CAMERA_SRI_TEMPLATE.replace("{n}", &n.to_string()))
        .collect();
    futures_util::future::join_all(
        camera_names
            .iter()
            .map(|name| ctx.command_send(name, "start_producing", Some(payload.clone()))),
    )
    .await;

    tokio::time::sleep(Duration::from_secs(timeout)).await;

    futures_util::future::join_all(
        camera_names
            .iter()
            .map(|name| ctx.command_send(name, "stop_producing", None)),
    )
    .await;

    DispatchOutcome {
        calls: DispatchedCalls::Discovered,
        expected_messages: load.saturating_mul(timeout),
    }
}

/// Re-publishes every rack host's `env_filter` (see [`swarm_telemetry::TOPIC_ENV_FILTER`] — every
/// runtime subscribes to this topic and hot-reloads its own `tracing_subscriber::EnvFilter` on
/// receipt) with `sorg_execution::wasm::cell::state::message_handler=info` added on top of the
/// filter `test_framework::rack::provision` already configured.
///
/// That module is where the `cell_task::event_batch`/`cell_task::event_dispatch` spans this
/// benchmark's `event_batch` report section reads actually live — not
/// `sorg_execution::wasm::cell::observability`, the only submodule the rack deployment's base
/// filter carves an exception for out of its blanket `sorg_execution=warn` — so without this,
/// those two spans get silently dropped on every rack run and the report's `event_batch` section
/// comes back `batches: 0` for every load, even though everything else (traces, hop coverage,
/// latency) stays correct. Fixed up here, benchmark-side, rather than in the shared rack
/// provisioning filter, since needing `event_batch` data is specific to this benchmark's own
/// report, not something every rack deployment should have to carry.
///
/// Pub/sub, not durable: a `put` sent before a given host's reload subscriber has declared itself
/// is simply never delivered to it, so this retries a few times over a short window rather than
/// publishing once and hoping every host's subscriber was already up in time.
async fn publish_rack_log_filter(session: &zenoh::Session) {
    const EXTRA_LOG_FILTER: &str = "info,h2=warn,sorg_execution=warn,sorg_execution::wasm::host_functions::logging=info,sorg_execution::wasm::cell::observability=info,sorg_execution::wasm::cell::state::message_handler=info,sorg_common=warn,db=warn,db_client=warn,wasmtime=off,cranelift_codegen=off,zenoh=off,swarm_telemetry=off,opentelemetry_sdk=off,hyper_util=off,rustls=off";

    for attempt in 0..5 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if let Err(err) = session
            .put(swarm_telemetry::TOPIC_ENV_FILTER, EXTRA_LOG_FILTER)
            .await
        {
            eprintln!("warning: failed to publish rack env_filter (attempt {attempt}): {err}");
        }
    }
}

struct WarehouseScenario;

/// Where each scope this benchmark writes should live: every cell's own scope on
/// the host that runs it, and the event bus on the host that consumes it.
///
/// One replica each, deliberately — a scope's writes go to a single holder
/// whatever its replica count, so more replicas would add catch-up traffic
/// without widening the write path. See `warehouse_benchmark::prealloc` for why
/// this is needed at all.
fn rack_replica_assignments(specialized: &Specialized) -> Vec<Assignment> {
    let mut assignments = Vec::new();

    let mut cell = |name: String, tag: String| {
        let sri =
            Sri::of_path(&name).unwrap_or_else(|err| panic!("invalid cell name {name:?}: {err}"));

        assignments.push(Assignment {
            selector: ReplicaSelector::Cell(sri),
            scope: scope_of_cell(sri),
            tag,
            label: name,
        });
    };

    cell(String::from(CENTRAL_SRI), String::from(RACK_CENTRAL_TAG));

    for n in 0..specialized.num_zones {
        cell(
            ZONE_SRI_TEMPLATE.replace("{n}", &n.to_string()),
            rack_zone_tag(n),
        );
    }

    for n in 0..specialized.num_objects {
        cell(
            ASSET_SRI_TEMPLATE.replace("{n}", &n.to_string()),
            rack_object_tag(n),
        );
    }

    // An internal producer runs on the harness's own untagged runtime, which no
    // host tag names — spread its private scopes over the object hosts so they
    // at least do not all land on one node.
    if specialized.producer.kind == ProducerKind::Internal {
        let spread = specialized.num_objects.max(1);
        for (n, sri) in producer_sris(specialized).into_iter().enumerate() {
            assignments.push(Assignment {
                selector: ReplicaSelector::Cell(sri),
                scope: scope_of_cell(sri),
                tag: rack_object_tag(n % spread),
                label: CAMERA_SRI_TEMPLATE.replace("{n}", &n.to_string()),
            });
        }
    }

    // No event-bus entry: the zone tier addresses central by SRI now, so the
    // only scopes this benchmark writes are cell scopes, every one of them
    // pinned to the host that runs its cell.
    assignments
}

impl BenchmarkScenario for WarehouseScenario {
    type Specialized = Specialized;

    fn title(&self) -> String {
        "Warehouse Benchmark Report".to_owned()
    }

    async fn build_ctx(&self, specialized: &Specialized) -> SwarmTestCtx {
        let mut builder = match &specialized.rack {
            None => {
                let config = specialized.config.clone().unwrap_or_else(|| {
                    PathBuf::from(test_framework::asset!("local_swarm.jsonnet"))
                });

                SwarmTest::builder()
                    .config(config)
                    .wasm_cell_with_api(TIER_3_CENTRAL_PATH, CENTRAL_SRI)
                    .wasm_cell_replicated_with_api(
                        TIER_2_ZONE_PATH,
                        ZONE_SRI_TEMPLATE,
                        specialized.num_zones,
                    )
                    .wasm_cell_replicated_with_api(
                        TIER_1_OBJECT_PATH,
                        ASSET_SRI_TEMPLATE,
                        specialized.num_objects,
                    )
            }
            Some(rack) => {
                let hosts = rack_host_specs(rack, specialized.num_zones, specialized.num_objects);
                let myrmic_path = rack.myrmic_path.clone();
                let zenoh_connect = rack.zenoh_connect.clone();

                let zone_tags: Vec<Vec<String>> = (0..specialized.num_zones)
                    .map(|n| vec![rack_zone_tag(n)])
                    .collect();
                let object_tags: Vec<Vec<String>> = (0..specialized.num_objects)
                    .map(|n| vec![rack_object_tag(n)])
                    .collect();

                SwarmTest::builder()
                    .provisioner(move || async move {
                        test_framework::rack::provision(
                            &hosts,
                            &myrmic_path,
                            &zenoh_connect,
                            // Files, not the db: this benchmark measures the
                            // datalayer, so its own telemetry must not compete
                            // with the load for it — with db export, every
                            // host's exporter wrote into the tele scope pinned
                            // to the central host, i.e. straight into the
                            // fan-in bottleneck under measurement.
                            test_framework::rack::RackTelemetry::Files {
                                dir: RACK_TELEMETRY_DIR,
                            },
                        )
                        .await
                    })
                    .wasm_cell_pinned_with_api(
                        TIER_3_CENTRAL_PATH,
                        CENTRAL_SRI,
                        vec![RACK_CENTRAL_TAG.to_owned()],
                    )
                    .wasm_cell_replicated_pinned_with_api(
                        TIER_2_ZONE_PATH,
                        ZONE_SRI_TEMPLATE,
                        &zone_tags,
                    )
                    .wasm_cell_replicated_pinned_with_api(
                        TIER_1_OBJECT_PATH,
                        ASSET_SRI_TEMPLATE,
                        &object_tags,
                    )
            }
        };

        if specialized.producer.kind == ProducerKind::Internal {
            // `wasm_cell_replicated` (plain `BuildTarget::Wasm`) passes myrmic `--target linux`,
            // which selects a crate's named `linux` *binary* target — meant for cells that
            // declare one per runtime, not this cell's single `cdylib` lib. `_with_api` builds
            // with no `--target`, which auto-selects the sole lib target instead.
            //
            // Runs on the harness's own default (untagged) runtime in the rack case too — the
            // rack topology doesn't currently assign a host to the internal producer, only to the
            // three cell tiers; see the `[specialized.rack]` doc comment.
            builder = builder.wasm_cell_replicated_with_api(
                INPUT_FIXED_CAMERA_PATH,
                CAMERA_SRI_TEMPLATE,
                specialized.producer.replicas,
            );
        }

        // Deferred rather than `start()`, so the replication configuration is in
        // place *before* any cell writes anything: a scope written before its
        // replica exists mints a fallback sink, and then reads and writes can
        // resolve to different holders while that sink drains.
        let spawned = builder.spawn().await;
        let mut ctx = spawned.connect_deferred().await;

        if let Some(rack) = &specialized.rack {
            publish_rack_log_filter(ctx.process().session()).await;

            // The rack is the only deployment where this matters: locally
            // there is one node, so there is nothing to spread onto and
            // nothing to fall back to.
            if rack.preallocate_replicas {
                preallocate(
                    ctx.process().session(),
                    &rack_replica_assignments(specialized),
                )
                .await;
            }
        }

        ctx.load_cells().await;

        ctx
    }

    fn run_params(&self, specialized: &Specialized) -> Vec<(String, String)> {
        let producer = match specialized.producer.kind {
            ProducerKind::External => "external".to_owned(),
            ProducerKind::Internal => {
                format!(
                    "internal (replicas={}, fixed_delay={})",
                    specialized.producer.replicas, specialized.producer.fixed_delay
                )
            }
        };
        vec![
            (
                "Num objects".to_owned(),
                specialized.num_objects.to_string(),
            ),
            ("Num zones".to_owned(), specialized.num_zones.to_string()),
            (
                "Fan-out strategy".to_owned(),
                specialized.fan_out_strategy.as_str().to_owned(),
            ),
            ("Producer".to_owned(), producer),
        ]
    }

    fn sri_names(&self, specialized: &Specialized) -> HashMap<Sri, String> {
        let mut names = HashMap::new();
        for (template, count) in [
            (ASSET_SRI_TEMPLATE, specialized.num_objects),
            (ZONE_SRI_TEMPLATE, specialized.num_zones),
        ] {
            for n in 0..count {
                let name = template.replace("{n}", &n.to_string());
                let sri = Sri::of_path(&name)
                    .unwrap_or_else(|err| panic!("invalid cell name {name:?}: {err}"));
                names.insert(sri, name);
            }
        }
        names.insert(
            Sri::of_path(CENTRAL_SRI).unwrap_or_else(|err| panic!("invalid central sri: {err}")),
            CENTRAL_SRI.to_owned(),
        );
        if specialized.producer.kind == ProducerKind::Internal {
            for (n, sri) in producer_sris(specialized).into_iter().enumerate() {
                names.insert(sri, CAMERA_SRI_TEMPLATE.replace("{n}", &n.to_string()));
            }
        }
        names
    }

    fn expected_hops(&self, specialized: &Specialized) -> Vec<Vec<Sri>> {
        let Topology {
            asset_sris,
            zone_sris,
            central_sri,
        } = topology(specialized.num_objects, specialized.num_zones);
        vec![asset_sris, zone_sris, vec![central_sri]]
    }

    async fn dispatch(
        &self,
        ctx: &SwarmTestCtx,
        specialized: &Specialized,
        pass_index: usize,
        load: u64,
        timeout: u64,
    ) -> DispatchOutcome {
        match specialized.producer.kind {
            ProducerKind::External => {
                dispatch_external(ctx, specialized, pass_index, load, timeout).await
            }
            ProducerKind::Internal => {
                dispatch_internal(ctx, specialized, pass_index, load, timeout).await
            }
        }
    }

    fn hop_coverage(
        &self,
        specialized: &Specialized,
        metrics_delta: &CellInteractionMetricsSnapshot,
    ) -> Vec<(String, u64)> {
        let Topology {
            asset_sris,
            zone_sris,
            central_sri,
        } = topology(specialized.num_objects, specialized.num_zones);

        let object = metrics_delta.matching_sri(&asset_sris);
        let zone = metrics_delta.matching_sri(&zone_sris);
        let central = metrics_delta.matching_sri(&[central_sri]);

        let mut rows = Vec::new();
        if specialized.producer.kind == ProducerKind::Internal {
            let producer = metrics_delta.matching_sri(&producer_sris(specialized));
            rows.push((
                "internal producer sent commands".to_owned(),
                producer.commands_sent,
            ));
        }
        rows.extend([
            (
                "tier 1 object processed commands".to_owned(),
                object.commands_received,
            ),
            (
                "tier 1 object sent commands".to_owned(),
                object.commands_sent,
            ),
            (
                "tier 2 zone processed commands".to_owned(),
                zone.commands_received,
            ),
            ("tier 2 zone sent commands".to_owned(), zone.commands_sent),
            (
                "tier 3 central processed commands".to_owned(),
                central.commands_received,
            ),
        ]);
        rows
    }
}

#[tokio::main]
async fn main() {
    bench_harness::run(WarehouseScenario).await;
}
