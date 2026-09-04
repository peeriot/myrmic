//! Deploying and driving a set of remote hosts (e.g. a rack of Raspberry Pis) as myrmic runtimes
//! over SSH, as an alternative to spawning a single local swarm process (see
//! [`crate::swarm::backend::local::LocalBinary`]).
//!
//! Each host runs its own self-contained `myrmic runtimes start` — a full zenoh peer with
//! orchestration/execution/db plugins baked in (`myrmic` links the `swarm` crate directly, see
//! `swarm/myrmic-cli/src/cmd/runtimes/start.rs`), tagged with its role. Peers discover each other
//! over the network (zenoh's default multicast scouting; fall back to a static `connect` list in
//! the uploaded config if multicast is blocked on the LAN). There is no separate "swarm router"
//! process to provision, and no per-cell asset upload: wasm cell artifacts are pushed into the
//! distributed class registry over zenoh itself (see [`crate::cell::CellArtifact::register_on`]),
//! not copied to disk. The only thing that genuinely needs `scp`ing is the `myrmic` binary itself.

use std::io::Write as _;
use std::path::Path;

use crate::myrmic::{Myrmic, MyrmicBackend as _, Runtime, SshBinary};
use crate::swarm::SwarmProcess;

/// One remote host's role in the deployment: which myrmic runtime to start there, and under
/// which capability tags. Pin cells to a specific host with
/// [`crate::scenario::SwarmTestBuilder::wasm_cell_replicated_pinned_with_api`], passing each
/// replica the `tags` of the [`HostSpec`] it should land on.
pub struct HostSpec {
    /// SSH destination, e.g. `peeriot@rack-node-3.peeriot.intra` (anything `ssh`/`scp` accept,
    /// including `~/.ssh/config` aliases)
    pub host: String,
    /// unique `myrmic runtimes start --name` for this host
    pub runtime_name: String,
    /// capability tags this host's runtime advertises
    pub tags: Vec<String>,
    /// pin this runtime's zenoh listen endpoint to a fixed TCP port instead of myrmic's default
    /// ephemeral one. Set this on whichever host `zenoh_connect` (see [`provision`]) points at —
    /// a driver reaching into the mesh from outside (e.g. through an SSH tunnel) needs a stable,
    /// known port to dial; hosts nothing dials into directly don't need this, since they still
    /// find each other over zenoh's own multicast scouting.
    pub listen_port: Option<u16>,
}

impl HostSpec {
    pub fn new(
        host: impl Into<String>,
        runtime_name: impl Into<String>,
        tags: Vec<String>,
    ) -> Self {
        Self {
            host: host.into(),
            runtime_name: runtime_name.into(),
            tags,
            listen_port: None,
        }
    }

    /// [`Self::new`], additionally pinning this host's runtime to listen on `port` — see
    /// [`Self::listen_port`].
    pub fn with_listen_port(mut self, port: u16) -> Self {
        self.listen_port = Some(port);
        self
    }
}

/// Upload the (cross-compiled, e.g. aarch64) `myrmic` binary at `myrmic_bin` to `remote_path` on
/// every host in `hosts`, creating its parent directory first and marking it executable.
///
/// Run once before [`provision`] for a fresh rack run. `myrmic` embeds the swarm library
/// directly, so nothing else needs deploying — see the module docs.
///
/// Uploads to every host concurrently rather than one at a time: this is bound by the driver
/// machine's own uplink to the hosts (each host gets an independent SSH/SCP connection, so
/// there's no shared resource on the *receiving* end for concurrent uploads to contend over),
/// and a single SCP stream commonly can't fill that link on its own — its throughput is capped
/// by TCP's per-connection window, which a higher-latency link (e.g. a VPN/WAN hop, vs. hosts on
/// the same LAN as the driver) shrinks well below the link's actual capacity. Several streams in
/// flight at once use more of the available bandwidth than any one of them could alone. Also
/// marks the local binary executable once up front and preserves that bit over `scp -p`, instead
/// of a separate remote `chmod` round trip per host.
pub async fn upload_binary(hosts: &[HostSpec], myrmic_bin: &Path, remote_path: &str) {
    set_executable(myrmic_bin).await;

    let uploads = hosts.iter().map(|host| async move {
        if let Some(dir) = Path::new(remote_path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            run_ssh(&host.host, &["mkdir", "-p", &dir.display().to_string()]).await;
        }
        run_scp_preserving_mode(myrmic_bin, &host.host, remote_path).await;
    });
    futures::future::join_all(uploads).await;
}

async fn set_executable(path: &Path) {
    let output = tokio::process::Command::new("chmod")
        .arg("+x")
        .arg(path)
        .output()
        .await
        .unwrap_or_else(|e| panic!("failed to run chmod +x {}: {e}", path.display()));
    assert!(
        output.status.success(),
        "chmod +x {} failed: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// How a provisioned rack deployment records telemetry.
#[derive(Debug, Clone, Copy)]
pub enum RackTelemetry<'a> {
    /// Every runtime exports telemetry into the shared `tele/telemetry` db
    /// scope, replication-pinned to the host advertising `holder_tag` — see
    /// `configure_telemetry_replication` for why the pin matters. Telemetry
    /// writes share the datalayer with the workload.
    Db { holder_tag: &'a str },
    /// Every runtime writes telemetry to JSON-lines files under `dir` on its
    /// own host (`swarm_telemetry::file`), fetched over SSH by the harness
    /// (`crate::telemetry_files`) — nothing telemetry-related touches the db,
    /// so a load benchmark's telemetry cannot perturb the datalayer it is
    /// measuring. `dir` is wiped on each host during provisioning, so one
    /// pass's files never bleed into the next.
    Files { dir: &'a str },
}

/// Start a tagged myrmic runtime on every host in `hosts` (the `myrmic` binary at `myrmic_path`,
/// uploaded via [`upload_binary`]), open an SSH tunnel to whichever host's [`HostSpec::listen_port`]
/// is set, and open a zenoh client session into the resulting mesh through that tunnel at
/// `zenoh_connect` (e.g. `tcp/127.0.0.1:<port>`, the local end of the tunnel).
///
/// `telemetry` picks where every runtime's telemetry lands — the shared db, or
/// per-host files — see [`RackTelemetry`].
///
/// The returned [`SwarmProcess`] owns every started [`Runtime`] and the tunnel — dropping it stops
/// each runtime (best-effort; see [`Runtime`]'s `Drop` impl) and kills the tunnel process, the
/// same way a locally-spawned swarm process is killed on drop. Callers whose run might be
/// interrupted before the [`SwarmProcess`] is dropped cleanly (e.g. a crashed harness) should
/// follow up with [`cleanup`].
pub async fn provision(
    hosts: &[HostSpec],
    myrmic_path: &str,
    zenoh_connect: &str,
    telemetry: RackTelemetry<'_>,
) -> SwarmProcess {
    // Opened up front, concurrently with runtime startup below (not awaited afterwards): `-L`
    // creates the local listening socket immediately and only forwards lazily per incoming
    // connection, so it doesn't matter that the remote host's zenoh listener isn't up yet — by
    // the time `open_client_session` below makes its first connection attempt, either the
    // runtimes are already up or its own retry loop (30s, see `open_client_session`) comfortably
    // outlasts the runtime startup this races against.
    let tunnel = open_ssh_tunnel(hosts, zenoh_connect);

    // Concurrent, not sequential — same rationale as `upload_binary`: each host is an
    // independent SSH round trip (config upload + runtime start), so provisioning N hosts one
    // at a time pays N times the round-trip latency for no benefit.
    let runtimes: Vec<Runtime<SshBinary>> =
        futures::future::join_all(hosts.iter().map(|host| async move {
            let myrmic = Myrmic::ssh_at(host.host.clone(), myrmic_path);
            let tag_refs: Vec<&str> = host.tags.iter().map(String::as_str).collect();

            // Serialize the pass boundary: the previous pass's Drop guard
            // issues `myrmic runtimes delete` without waiting for the remote
            // process to exit, and a runtime restarted under the same name
            // reuses its stable id — so a still-draining predecessor and this
            // pass's runtime briefly share an identity in one multicast-scouted
            // mesh, and this pass's deploys race its dying custody/registry
            // state (`NoRuntimesAvailable` at pass boundaries; the 2026-08-28
            // force-flush diagnosis, recommendation 1). Wait for the old
            // process to actually be gone before starting its successor.
            wait_for_runtime_exit(&host.host, myrmic_path).await;

            // Telemetry files persist across runtime restarts (like the
            // insert log), so a leftover metrics-latest from a previous run
            // would feed stale counters into this one's first snapshot.
            if let RackTelemetry::Files { dir } = telemetry {
                run_ssh(&host.host, &["rm", "-rf", dir]).await;
            }

            let remote_config = host_config_path(myrmic_path, &host.runtime_name);
            upload_host_config(&host.host, host.listen_port, &remote_config, telemetry).await;
            myrmic
                .start_runtime_at(&host.runtime_name, &tag_refs, Some(&remote_config))
                .await
        }))
        .await;

    let session = crate::swarm::open_client_session(zenoh_connect).await;

    if let RackTelemetry::Db { holder_tag } = telemetry {
        configure_telemetry_replication(hosts, myrmic_path, holder_tag, &session).await;
    }

    wait_for_runtime_registry(&session, hosts.len()).await;

    let process = SwarmProcess::new((runtimes, tunnel), session);
    match telemetry {
        RackTelemetry::Files { dir } => {
            let hosts = hosts.iter().map(|host| host.host.clone()).collect();
            process.with_telemetry_files(crate::telemetry_files::TelemetryFiles::new(
                hosts,
                dir.to_owned(),
            ))
        }
        RackTelemetry::Db { .. } => process,
    }
}

/// Blocks until no process spawned from `myrmic_path` is left running on `host` — see the call
/// site in [`provision`] for why a pass boundary must wait out its predecessor's exit. Gives up
/// with a warning after 30 s and proceeds (the run is then no worse off than before this wait
/// existed). `pgrep -f` matches the daemonized runtime by its binary path; it exits non-zero when
/// nothing matches, so the remote `|| true` keeps ssh's own failures distinguishable. The
/// pattern is anchored (`^`) so it matches only command lines *starting* with the binary path —
/// unanchored, it would match the remote shell running the check itself (whose own command line
/// contains the pattern) and never see an empty result.
async fn wait_for_runtime_exit(host: &str, myrmic_path: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);

    loop {
        let mut command = tokio::process::Command::new("ssh");
        command.arg("-o").arg("BatchMode=yes");
        if let Some(identity_file) = crate::ssh_identity_file() {
            command.arg("-i").arg(identity_file);
        }
        if let Some(known_hosts_file) = crate::ssh_known_hosts_file() {
            command
                .arg("-o")
                .arg(format!("UserKnownHostsFile={known_hosts_file}"));
        }

        let output = command
            .arg(host)
            .arg(format!("pgrep -f '^{myrmic_path}' || true"))
            .output()
            .await;

        match output {
            Ok(output) if output.status.success() => {
                if output.stdout.iter().all(u8::is_ascii_whitespace) {
                    return;
                }
            }
            // ssh itself failing is not proof a runtime lingers; warn and let
            // the timeout decide.
            Ok(output) => {
                eprintln!(
                    "warning: runtime-exit check on {host} failed (exit {:?}): {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(err) => {
                eprintln!("warning: unable to run runtime-exit check on {host}: {err}");
            }
        }

        if tokio::time::Instant::now() >= deadline {
            eprintln!(
                "warning: a previous runtime on {host} is still running after 30s; \
                 provisioning anyway — expect pass-boundary races"
            );
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Blocks until the exec registry lists `expected` runtimes and each of them has a node-lease
/// row — the same two reads the orchestrator's placement preprocessing does before it will
/// place anything (see `sorg-orchestration`'s `Placement::read`).
///
/// Runtime registration/lease writes and the driver's first deploy are separate routed
/// transactions with nothing ordering them; the gap used to be papered over by the driver's own
/// slowness. With the rack nodes on the `performance` cpufreq governor (2026-08-28), the driver
/// reaches its first deploy before the registry rows have converged to whichever node the
/// placement read lands on, and the deploy dies with `NoRuntimesAvailable` (run 33191013614 —
/// notably on the *second* pass, whose fresh runtimes re-register from scratch). Like
/// `configure_telemetry_replication`'s canary, poll the real read path rather than sleeping.
///
/// Times out after 60s with a warning and proceeds — the deploy that follows will then fail
/// with the precise placement error, which is louder than anything this could construct.
async fn wait_for_runtime_registry(session: &zenoh::Session, expected: usize) {
    let start = tokio::time::Instant::now();
    let converged = crate::wait_until(
        std::time::Duration::from_mins(1),
        std::time::Duration::from_millis(250),
        || async {
            let execs = sorg_common::exec_registry::list_registered_execs(session)
                .await
                .unwrap_or_default();
            if execs.len() < expected {
                return false;
            }
            let leases = sorg_common::node_lease::list_leases(session)
                .await
                .unwrap_or_default();
            execs
                .iter()
                .filter(|exec| leases.iter().any(|(id, _)| *id == exec.id()))
                .count()
                >= expected
        },
    )
    .await;

    if converged {
        eprintln!(
            "runtime registry converged: {expected} exec(s) registered and leased, {:?}",
            start.elapsed()
        );
    } else {
        eprintln!(
            "warning: gave up waiting for {expected} registered+leased exec runtime(s) after \
             {:?}; proceeding anyway — the next deploy will name what is missing",
            start.elapsed()
        );
    }
}

/// Opens an SSH local port forward (`ssh -L <local>:127.0.0.1:<remote> host`) from `zenoh_connect`'s
/// `tcp/127.0.0.1:<port>` to whichever host in `hosts` has [`HostSpec::listen_port`] set — the
/// `zenoh_connect` client session [`provision`] opens right after this reaches the mesh through
/// this tunnel rather than dialing a rack host directly (these hosts are typically only reachable
/// from inside their own network, not from wherever this test-framework process runs).
///
/// # Panics
///
/// Panics if no host has `listen_port` set, if `zenoh_connect` has no trailing `:<port>`, or if
/// `ssh` itself can't be spawned (a non-zero exit *after* spawning, e.g. a rejected connection,
/// isn't caught here — that surfaces once `open_client_session`'s own retries against the tunneled
/// port exhaust).
fn open_ssh_tunnel(hosts: &[HostSpec], zenoh_connect: &str) -> tokio::process::Child {
    let tunnel_host = hosts
        .iter()
        .find(|h| h.listen_port.is_some())
        .unwrap_or_else(|| {
            panic!(
                "provision: no host in `hosts` has a `listen_port` set (see \
             `HostSpec::with_listen_port`) to open the `{zenoh_connect}` SSH tunnel through"
            )
        });
    let remote_port = tunnel_host.listen_port.expect("checked by find above");
    let local_port = zenoh_connect.rsplit(':').next().unwrap_or_else(|| {
        panic!("provision: zenoh_connect {zenoh_connect:?} has no `:<port>` suffix to tunnel to")
    });

    let mut command = tokio::process::Command::new("ssh");
    command
        .arg("-N")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-L")
        .arg(format!("{local_port}:127.0.0.1:{remote_port}"));
    if let Some(identity_file) = crate::ssh_identity_file() {
        command.arg("-i").arg(identity_file);
    }
    if let Some(known_hosts_file) = crate::ssh_known_hosts_file() {
        command
            .arg("-o")
            .arg(format!("UserKnownHostsFile={known_hosts_file}"));
    }
    command
        .arg(&tunnel_host.host)
        .kill_on_drop(true)
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn ssh tunnel to {}: {e}", tunnel_host.host))
}

/// Designates the host advertising `tag` as the sole replication holder for the `tele/telemetry`
/// scope every runtime's telemetry exporter writes spans/logs/metrics into.
///
/// Unlike `sys`/`sorg`/`gw`, `tele/telemetry` isn't unconditionally replicated to every node —
/// without this, each host's exporter independently claims it (observed directly: every host
/// logging its own `announcing offload of tele/telemetry/p`, with no convergence between them),
/// and writes contend forever with no consensus on who actually owns the scope. That's what was
/// behind `force_flush_telemetry`'s writes consistently timing out on a rack deployment — a
/// single local swarm process never hits this, since there's only ever one telemetry exporter to
/// begin with. Configuring replication once, here, before anything touches telemetry, fixes it.
async fn configure_telemetry_replication(
    hosts: &[HostSpec],
    myrmic_path: &str,
    tag: &str,
    session: &zenoh::Session,
) {
    let Some(host) = hosts.first() else {
        return;
    };
    run_ssh(
        &host.host,
        &[myrmic_path, "replicate", "scope:tele", "-t", tag],
    )
    .await;

    // The replication config entry itself reaches every host quickly, via gossip over the
    // unconditionally-replicated `sys` namespace. But that alone isn't enough: each host's own
    // `swarm/plugins/db/replica_sets.rs::run` is what actually *acts* on the entry (starting a
    // real replicator toward `tag`), and per that module's own doc comment, "entries arriving by
    // replication raise no local table event, so the poll — not the subscription — is what makes
    // a remote change take effect" — its `POLL_INTERVAL` is a hardcoded 30s. On top of that, any
    // telemetry write that lands before `tag` has actually started replicating falls back to a
    // *provisional* per-transaction offloader election on whichever node it happened to reach —
    // and per `swarm/plugins/db.rs`'s offload loop, that data only escalates to a durable,
    // consistently-locatable replica after its own `DEFAULT_OFFLOAD_ESCALATION_TIMEOUT` (another
    // hardcoded 30s) of remaining uncovered. These two delays can compound (observed empirically:
    // ~76s before a write became visible to a query on the very node holding it), so a fixed
    // sleep tuned to one of them isn't reliably enough for the other. Instead, poll until a
    // routed write/read round-trip through this exact scope actually succeeds — confirming the
    // whole path the real telemetry exporters depend on is live, not guessing at how long that
    // takes.
    let client = db_client::v1::Client::new(session);
    let scope = swarm_telemetry::db::scope();
    // `tb_insert`/`tb_list` on `TABLE_TRACES`, not `key_put`/`key_get` — this needs to exercise
    // exactly the same storage path the real trace/log exporter uses (`insert_batch` in
    // `swarm-telemetry/src/db/mod.rs`), since a simple key/value round-trip converging quickly
    // wouldn't prove anything about whether a *table* write on this scope has too.
    let canary_marker = format!("rack-replication-canary-{}", uuid::Uuid::new_v4());
    let canary_value = canary_marker.clone().into_bytes();

    let start = tokio::time::Instant::now();
    let deadline = start + std::time::Duration::from_mins(2);
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let write_ok = client
            .write_tx_in(scope.clone(), {
                let canary_value = canary_value.clone();
                async move |c, tx_id| {
                    c.send(db_client::v1::models::tb_insert_batched::Request {
                        id: tx_id,
                        op: db_client::v1::models::tb_insert_batched::Op {
                            scope: swarm_telemetry::db::scope(),
                            table: swarm_telemetry::db::TABLE_TRACES.to_owned(),
                            entries: vec![(None, canary_value)],
                        },
                    })
                    .await
                }
            })
            .await
            .is_ok_and(|res| res.is_ok());

        let found = write_ok
            && client
                .read_tx_in(scope.clone(), async move |c, tx_id| {
                    c.send(db_client::v1::models::tb_list::Request {
                        id: tx_id,
                        op: db_client::v1::models::tb_list::Op {
                            scope: swarm_telemetry::db::scope(),
                            table: swarm_telemetry::db::TABLE_TRACES.to_owned(),
                            cursor: None,
                            limit: None,
                            order: None,
                        },
                    })
                    .await
                })
                .await
                .is_ok_and(|res| {
                    res.is_ok_and(|resp| {
                        resp.entities
                            .iter()
                            .any(|(_, value)| value.as_slice() == canary_value.as_slice())
                    })
                });

        if found {
            eprintln!(
                "tele replication converged on {tag} after {attempts} attempt(s), {:?}",
                start.elapsed()
            );
            return;
        }

        if tokio::time::Instant::now() >= deadline {
            eprintln!(
                "warning: gave up waiting for tele replication to converge on {tag} after \
                 {attempts} attempt(s), {:?}; proceeding anyway — telemetry may still be \
                 unreliable this run",
                start.elapsed()
            );
            return;
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

fn host_config_path(myrmic_path: &str, runtime_name: &str) -> String {
    format!("{myrmic_path}-{runtime_name}.yaml")
}

/// Write a `SwarmConfig` YAML for `host` and upload it to `remote_path`, always shortening the
/// telemetry export intervals and, when `listen_port` is given, additionally pinning the zenoh
/// listen endpoint to that port on all interfaces (the same override the ESP32 HIL tests apply
/// via jsonnet — `embedded/hil-tests/tests/data/swarm.jsonnet`'s `tcp/[::]:7447` — just fed to
/// `myrmic runtimes start <path>` in YAML instead).
///
/// The `OTel` SDK's defaults (60s periodic metric export, 5s log/trace batch delay) are tuned for
/// long-running services, not a benchmark whose whole load pass plus drain window can be over in
/// well under 60s — without this, a run can finish, tear down, and report every metric-derived
/// field as empty/zero, not because anything is broken but because the exporter's first periodic
/// tick simply never arrived in time. `force_flush_telemetry` exists to bypass this by forcing an
/// export on demand, but its query has been observed to go unanswered on a rack deployment (see
/// its doc comment), so this is a second, independent way to keep exports within the benchmark's
/// own time budget even when force-flush doesn't come through.
async fn upload_host_config(
    host: &str,
    listen_port: Option<u16>,
    remote_path: &str,
    telemetry: RackTelemetry<'_>,
) {
    use std::fmt::Write as _;

    const METRICS_EXPORT_INTERVAL_MS: u64 = 2_000;
    const LOG_TRACE_BATCH_DELAY_MS: u64 = 1_000;
    // The OTel SDK's default span queue (2048) silently drops spans when a
    // burst outruns the 1s export cadence — at ≥400 calls/s the fan-in host
    // produces several thousand spans inside a pass's dispatch window, and run
    // 33243476339 lost ~750 hop-2 spans at load 400 to exactly this (sporadic,
    // not load-proportional: whether the queue happens to fill between two
    // exports). Spans are local file appends on the rack now, so a deeper
    // queue and bigger export batches cost memory and file-write size, not
    // datalayer pressure.
    const TRACE_MAX_QUEUE_SIZE: usize = 16_384;
    const TRACE_MAX_EXPORT_BATCH_SIZE: usize = 2_048;
    // Explicitly overrides `myrmic-cli`'s own default filter (`swarm/myrmic-cli/src/utils.rs`'s
    // `build_filter`) rather than changing that default, since it's otherwise a reasonable,
    // widely-used default for interactive CLI use — this is the same filter, minus the blanket
    // `sorg_execution=warn` silently disabling `sorg_execution::wasm::cell::observability`'s
    // `cell_task::message_handler` span (level `info`): a *disabled* tracing span still gets
    // entered/exited, but `is_disabled()` short-circuits `begin_observability`'s `set_parent`
    // call, so it never adopts the incoming distributed-tracing context — its own span context
    // stays empty/invalid, and that's what gets propagated onward. The cell still runs and
    // messages still get delivered fine either way, so nothing about this is visible short of
    // the exported trace data itself silently containing an unrelated id per hop instead of one
    // shared id per call — worth carrying explicitly here since silent trace loss is exactly
    // what a benchmark measuring hop coverage needs `observability` audible for.
    const LOG_FILTER: &str = "info,h2=warn,sorg_execution=warn,sorg_execution::wasm::host_functions::logging=info,sorg_execution::wasm::cell::observability=info,sorg_common=warn,db=warn,db_client=warn,wasmtime=off,cranelift_codegen=off,zenoh=off,swarm_telemetry=off,opentelemetry_sdk=off,hyper_util=off,rustls=off";

    let sink = match telemetry {
        RackTelemetry::Db { .. } => "db_retention: \"1h\"".to_owned(),
        RackTelemetry::Files { dir } => format!("file_export_dir: \"{dir}\""),
    };

    let mut yaml = format!(
        "telemetry:\n\
         \x20\x20{sink}\n\
         \x20\x20logs:\n\
         \x20\x20\x20\x20env_filter: \"{LOG_FILTER}\"\n\
         \x20\x20\x20\x20batch:\n\
         \x20\x20\x20\x20\x20\x20scheduled_delay_ms: {LOG_TRACE_BATCH_DELAY_MS}\n\
         \x20\x20metrics:\n\
         \x20\x20\x20\x20export_interval_ms: {METRICS_EXPORT_INTERVAL_MS}\n\
         \x20\x20traces:\n\
         \x20\x20\x20\x20batch:\n\
         \x20\x20\x20\x20\x20\x20scheduled_delay_ms: {LOG_TRACE_BATCH_DELAY_MS}\n\
         \x20\x20\x20\x20\x20\x20max_queue_size: {TRACE_MAX_QUEUE_SIZE}\n\
         \x20\x20\x20\x20\x20\x20max_export_batch_size: {TRACE_MAX_EXPORT_BATCH_SIZE}\n"
    );
    if let Some(port) = listen_port {
        write!(
            yaml,
            "zenoh:\n  listen:\n    endpoints:\n      peer:\n        - \"tcp/[::]:{port}\"\n"
        )
        .expect("writing to a String cannot fail");
    }

    let mut file = tempfile::NamedTempFile::new().expect("failed to create tempfile");
    file.write_all(yaml.as_bytes())
        .expect("failed to write host config");
    run_scp(file.path(), host, remote_path).await;
}

/// Best-effort: stop any runtime still running on `hosts` and remove the uploaded binary, so a
/// repeated rack run starts from a clean slate even after an interrupted previous run.
///
/// This does not touch `myrmic runtimes start`'s persisted per-runtime state under
/// `$XDG_DATA_HOME/myrmic` on each host — a runtime started with the same `--name` again reuses
/// its stable id and any non-`--tmp` database, which is the desired behavior for retries; wipe
/// that directory over SSH separately if a run needs a truly empty database.
pub async fn cleanup(hosts: &[HostSpec], myrmic_path: &str) {
    // Concurrent, not sequential — same rationale as `provision`: each host's teardown (a
    // blocking `runtimes delete` plus two `rm -f`s) is an independent SSH round trip, so
    // cleaning up N hosts one at a time pays N times the round-trip latency for no benefit.
    futures::future::join_all(hosts.iter().map(|host| async move {
        let backend = SshBinary::at(host.host.clone(), myrmic_path);
        let runtime_name = host.runtime_name.clone();
        // The common case: `SwarmProcess`'s `Runtime` drop-guard (`myrmic/mod.rs`) already
        // deleted every runtime when the driver process exited normally, so `rack-ctl cleanup`
        // finding nothing left here — `myrmic runtimes delete`'s own "no runtime ... found"
        // message — isn't a failure, it's this backstop confirming there was nothing left to do.
        // It only matters as a backstop for a driver that never got to run its own cleanup (a
        // crash, a kill -9), so a genuine failure (host unreachable, permission denied, ...)
        // still needs to be loud.
        let delete_result =
            tokio::task::spawn_blocking(move || backend.delete_runtime_blocking(&runtime_name))
                .await
                .expect("delete_runtime_blocking task panicked");
        if let Err(err) = delete_result
            && !err.contains("no runtime")
        {
            eprintln!(
                "rack cleanup: failed to delete runtime `{}` on {}: {err}",
                host.runtime_name, host.host
            );
        }

        let config_path = host_config_path(myrmic_path, &host.runtime_name);
        futures::future::join(
            run_ssh(&host.host, &["rm", "-f", myrmic_path]),
            run_ssh(&host.host, &["rm", "-f", &config_path]),
        )
        .await;
    }))
    .await;
}

async fn run_ssh(host: &str, args: &[&str]) {
    let identity_file = crate::ssh_identity_file();
    let mut command = tokio::process::Command::new("ssh");
    if let Some(identity_file) = &identity_file {
        command.arg("-i").arg(identity_file);
    }
    if let Some(known_hosts_file) = crate::ssh_known_hosts_file() {
        command
            .arg("-o")
            .arg(format!("UserKnownHostsFile={known_hosts_file}"));
    }
    let output = command
        .arg(host)
        .args(args)
        .output()
        .await
        .unwrap_or_else(|e| panic!("failed to run ssh {host} {args:?}: {e}"));
    assert!(
        output.status.success(),
        "ssh {}{host} {args:?} (exit {:?}) failed\nstdout: {}\nstderr: {}",
        identity_file
            .as_deref()
            .map(|f| format!("-i {f} "))
            .unwrap_or_default(),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn run_scp(local: &Path, host: &str, remote_path: &str) {
    run_scp_impl(local, host, remote_path, false).await;
}

/// [`run_scp`], additionally preserving `local`'s mode bits on the uploaded copy (`scp -p`) —
/// for a file that's already executable locally and needs to land that way remotely too, without
/// a separate remote `chmod` round trip.
async fn run_scp_preserving_mode(local: &Path, host: &str, remote_path: &str) {
    run_scp_impl(local, host, remote_path, true).await;
}

async fn run_scp_impl(local: &Path, host: &str, remote_path: &str, preserve_mode: bool) {
    let local_display = local.display();
    let destination = format!("{host}:{remote_path}");
    let mut cmd = tokio::process::Command::new("scp");
    if preserve_mode {
        cmd.arg("-p");
    }
    let identity_file = crate::ssh_identity_file();
    if let Some(identity_file) = &identity_file {
        cmd.arg("-i").arg(identity_file);
    }
    if let Some(known_hosts_file) = crate::ssh_known_hosts_file() {
        cmd.arg("-o")
            .arg(format!("UserKnownHostsFile={known_hosts_file}"));
    }
    let output = cmd
        .arg(local)
        .arg(&destination)
        .output()
        .await
        .unwrap_or_else(|e| panic!("failed to run scp {local_display} {destination}: {e}"));
    assert!(
        output.status.success(),
        "scp {}{local_display} {destination} (exit {:?}) failed\nstdout: {}\nstderr: {}",
        identity_file
            .as_deref()
            .map(|f| format!("-i {f} "))
            .unwrap_or_default(),
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
