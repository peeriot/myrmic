use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
};

use cell_protocol::Sri;
use swarm_telemetry::db::opentelemetry_proto::tonic::{
    common::v1::{KeyValue, any_value::Value},
    metrics::v1::{Metric, NumberDataPoint, metric::Data, number_data_point},
};

const CELL_COMMANDS_PROCESSED: &str = "cell_commands_processed";
const CELL_EVENTS_PROCESSED: &str = "cell_events_processed";
const CELL_COMMANDS_SENT: &str = "cell_commands_sent";
const CELL_COMMANDS_FAILED: &str = "cell_commands_failed";
const CELL_MAILBOX_POLLS: &str = "cell_mailbox_polls";
const CELL_MAILBOX_EMPTY_POLLS: &str = "cell_mailbox_empty_polls";
const CELL_MAILBOX_BACKSTOP_POLLS: &str = "cell_mailbox_backstop_polls";
const CELL_MAILBOX_BACKSTOP_EMPTY_POLLS: &str = "cell_mailbox_backstop_empty_polls";
const CELL_MAILBOX_NOTIFICATIONS: &str = "cell_mailbox_notifications";
const CELL_COMMANDS_DELIVERED_POKE: &str = "cell_commands_delivered_poke";
const CELL_COMMANDS_DELIVERED_BACKSTOP: &str = "cell_commands_delivered_backstop";
const CELL_COMMIT_NANOS: &str = "cell_commit_nanos";
const CELL_COMMITS: &str = "cell_commits";
const CELL_PEEK_NANOS: &str = "cell_peek_nanos";
const CELL_PEEKS: &str = "cell_peeks";
const CELL_WAIT_NANOS: &str = "cell_wait_nanos";
const CELL_WAITS: &str = "cell_waits";
const CELL_BACKSTOP_WAIT_NANOS: &str = "cell_backstop_wait_nanos";
const CELL_BACKSTOP_WAITS: &str = "cell_backstop_waits";
const CELL_READ_FAILURES: &str = "cell_read_failures";
const CELL_EVENTS_SENT: &str = "cell_events_sent";
const CELL_MAILBOX_DEPTH_SUM: &str = "cell_mailbox_depth_sum";
const CELL_MAILBOX_DEPTH_SAMPLES: &str = "cell_mailbox_depth_samples";
const CELL_RECV_LAG_NANOS: &str = "cell_recv_lag_nanos";
const CELL_RECV_LAGS: &str = "cell_recv_lags";
const CELL_DISPATCH_NANOS: &str = "cell_dispatch_nanos";
const CELL_DISPATCHES: &str = "cell_dispatches";
const CELL_TURN_NANOS: &str = "cell_turn_nanos";
const CELL_TURNS: &str = "cell_turns";
const CELL_EXPORT_LOOKUP_NANOS: &str = "cell_export_lookup_nanos";
const CELL_EXPORT_LOOKUPS: &str = "cell_export_lookups";
const CELL_GUEST_CALL_NANOS: &str = "cell_guest_call_nanos";
const CELL_GUEST_CALLS: &str = "cell_guest_calls";
const CELL_SPAN_NANOS: &str = "cell_span_nanos";
const CELL_SPANS: &str = "cell_spans";
const CELL_HOST_LOG_NANOS: &str = "cell_host_log_nanos";
const CELL_HOST_LOGS: &str = "cell_host_logs";
const REPL_MSGS_SENT: &str = "repl_msgs_sent";
const REPL_MSGS_RECV: &str = "repl_msgs_recv";
const REPL_ANNOUNCE_HEADS: &str = "repl_announce_heads";
const REPL_ANNOUNCE_BASELINES: &str = "repl_announce_baselines";
const REPL_ANNOUNCE_SCOPES: &str = "repl_announce_scopes";
const REPL_HANDLE_QUEUE_NANOS: &str = "repl_handle_queue_nanos";
const REPL_HANDLE_NANOS: &str = "repl_handle_nanos";
const REPL_HANDLED: &str = "repl_handled";
const REPL_APPLIED: &str = "repl_applied";
const REPL_APPLIED_AGE_NANOS: &str = "repl_applied_age_nanos";
const REPL_APPLIED_AGE_SKEWED: &str = "repl_applied_age_skewed";
const REPL_PULLS: &str = "repl_pulls";
const REPL_PULL_NANOS: &str = "repl_pull_nanos";
const REPL_PULL_CHUNKS: &str = "repl_pull_chunks";
const REPL_SERVED_PULLS: &str = "repl_served_pulls";
const REPL_SERVED_CHUNKS: &str = "repl_served_chunks";
const REPL_SERVED_AGE_NANOS: &str = "repl_served_age_nanos";
const REPL_SERVED_AGE_SKEWED: &str = "repl_served_age_skewed";
const REPL_SERVED_PAGES_FULL: &str = "repl_served_pages_full";
const DB_PEEKS_SERVED: &str = "db_peeks_served";
const DB_PEEK_ROWS_SERVED: &str = "db_peek_rows_served";

/// Cell command/event counters aggregated by SRI.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CellInteractionMetrics {
    pub commands_received: u64,
    pub commands_sent: u64,
    /// Handler invocations that failed and rolled back, so the command was
    /// redelivered. Retry volume, not delivery: excluded from
    /// `commands_received` so [`CellInteractionMetricsSnapshot::loss`] means something.
    pub commands_failed: u64,
    /// Mailbox reads, and how many found nothing. A high empty share means a
    /// wake is arriving before the row is visible to the read — see
    /// `cell_mailbox::CommandMetrics`.
    pub mailbox_polls: u64,
    pub mailbox_empty_polls: u64,
    /// The same pair for reads driven by the 5s backstop rather than a poke.
    pub mailbox_backstop_polls: u64,
    pub mailbox_backstop_empty_polls: u64,
    /// Table events delivered to mailbox watchers, counted before `Notify`
    /// collapses them into wakeups. One append into a mailbox publishes exactly
    /// one event and deletes publish none, so this short of
    /// `commands_received` is the rate at which pokes are lost in transit.
    pub mailbox_notifications: u64,
    /// Which signal got each command moving. Table events and replication
    /// announces are both zenoh pushes, dropped rather than queued when a link
    /// congests; a row whose poke was dropped waits out the 5s backstop, so a
    /// large backstop share is that loss showing up as latency.
    pub commands_delivered_poke: u64,
    pub commands_delivered_backstop: u64,
    /// Wall time in the commit round trip a handler's transaction costs, and
    /// how many were made. A handler runs in microseconds, so the mean of
    /// these is what a cell's throughput ceiling is made of.
    pub commit_nanos: u64,
    pub commits: u64,
    /// The same for the `tb_peek` round trip that reads the mailbox.
    pub peek_nanos: u64,
    pub peeks: u64,
    /// How long the mailbox loop sat parked, split by what woke it. Rows arrive
    /// at the recipient's holder within 24ms while a hop takes 1600ms, so this
    /// is where the wait actually is: a long notified wait means the cell is not
    /// being poked even though the data is there.
    pub wait_nanos: u64,
    pub waits: u64,
    pub backstop_wait_nanos: u64,
    pub backstop_waits: u64,
    /// Reads that errored rather than returning empty — indistinguishable from
    /// an idle poll in the loop's behaviour, so counted separately.
    pub read_failures: u64,
    /// The mailbox depth the peek-serving node reported, sampled every 16th
    /// read: total over the samples and how many were taken. A mean far above
    /// the dispatched batch size means the reader's window is jammed with rows
    /// it already dispatched whose deletes have not landed where it reads; a
    /// mean near the batch size means the rows are simply not there yet.
    pub mailbox_depth_sum: u64,
    pub mailbox_depth_samples: u64,
    /// The three timers splitting one command's turn through the cell task —
    /// see `sorg_execution`'s `record_turn_split`: `recv_lag` is the producer
    /// send → loop receive hop (the cell task's run-queue wait), `dispatch`
    /// the handler call's wall clock (fuel-yield suspensions included), and
    /// `turn` the producer's whole send → handled round trip. Turn minus
    /// recv-lag, dispatch and commit is time nothing else accounts for.
    pub turn: TurnSplit,
    pub events_received: u64,
    pub events_sent: u64,
}

/// Commands/events sent but never received by a cell during a delta.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LossReport {
    pub commands_lost: u64,
    pub events_lost: u64,
}

impl LossReport {
    #[must_use]
    pub fn any(&self) -> bool {
        self.commands_lost > 0 || self.events_lost > 0
    }
}

/// Latest cell command/event counters keyed by SRI.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CellInteractionMetricsSnapshot {
    pub cells: BTreeMap<Sri, CellInteractionMetrics>,
}

/// Replication wire volumes, summed across every node in the mesh.
///
/// Node-level rather than per-cell, so unlike [`CellInteractionMetrics`] these
/// carry no `sri` and are aggregated whole. What they are for: a cell's outgoing
/// append lands on the *sender's* holder and reaches the recipient only once
/// replication drains it, and that drain measured 1.6s at load 1000 while the
/// recipient sat 94% idle. Chunks per pull is the discriminator — a stream
/// pulling one commit at a time is keeping up, while one pulling sixty has left
/// them waiting to be told about.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReplicationMetrics {
    pub announces_sent: u64,
    pub announces_recv: u64,
    pub changesets_sent: u64,
    pub changesets_recv: u64,
    /// Shape of the announces published: scopes covered, explicit heads
    /// carried, and how many scopes elided anything behind a baseline.
    /// `plan_catchup` walks every head on every receipt, so heads per announce
    /// is what makes an announce expensive.
    pub announce_scopes: u64,
    pub announce_heads: u64,
    pub announce_baselines: u64,
    /// Announce handling split into the wait and the work: `queue` is how long
    /// a spawned handler sat before running, `nanos` how long it then took.
    /// Announce work is cheap and announce volume is not, so a wait far larger
    /// than the work means an announce costs a turn on a busy runtime rather
    /// than any real computation.
    pub announce_queue_nanos: u64,
    pub announce_nanos: u64,
    pub announces_handled: u64,
    /// How old pulled rows were on arrival, against how many arrived. The
    /// bisection for the wait: an age in milliseconds means the data arrives
    /// promptly and the recipient is not looking, an age in seconds means
    /// arrival is what is slow.
    pub applied: u64,
    pub applied_age_nanos: u64,
    /// Arrivals whose age came out negative because the receiver's clock trails
    /// the writer's. These contribute nothing to the age total, so a large share
    /// means the mean age is measuring only the subset with favourable skew.
    pub applied_age_skewed: u64,
    pub pulls: u64,
    pub pull_nanos: u64,
    pub pull_chunks: u64,
    /// Serve-side view of the pulls, split into cell-scope traffic and
    /// everything else so the mailbox streams are not diluted by `sorg` noise.
    /// The age here is stamped and measured on the serving node's own clock —
    /// the honest side; the receiver-side `applied_age` rides the receiver's
    /// HLC, which a trailing physical clock pins to the stream it is applying.
    pub served_cells: ServedPulls,
    pub served_other: ServedPulls,
    /// Cell-scope pulls split by the puller's role. Rows leave their writer
    /// within ~20ms of commit, so whether the prompt puller is the scope's
    /// locate-visible replica or a fellow offloader decides whether a pulled
    /// row is a *delivered* row — reads can only route to the former.
    pub pulls_replica_cells: RolePulls,
    pub pulls_offload_cells: RolePulls,
    /// Served cell-scope peeks split by the serving node's role: how often
    /// mailbox reads actually land on a replica versus something else.
    pub peeks_replica_cells: RolePeeks,
    pub peeks_other_cells: RolePeeks,
}

/// The turn-split timers, one command each — see the `turn` field.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TurnSplit {
    pub recv_lag_nanos: u64,
    pub recv_lags: u64,
    pub dispatch_nanos: u64,
    pub dispatches: u64,
    pub turn_nanos: u64,
    pub turns: u64,
    /// The dispatch wall's own parts: `Instance::get_typed_func` (an export
    /// name lookup and signature check, per command) and the `call_async` wall
    /// around it, which is fiber entry plus the guest and its host calls.
    /// Dispatch minus the two is everything before the call — argument storage
    /// and the export name.
    pub export_lookup_nanos: u64,
    pub export_lookups: u64,
    pub guest_call_nanos: u64,
    pub guest_calls: u64,
    /// The observability span over one turn: opening it (creation, remote
    /// parent, the enter that starts it) plus closing it. Outside dispatch,
    /// inside the turn.
    pub span_nanos: u64,
    pub spans: u64,
    /// The `log` host call, guest entry to return — the guest's own share of
    /// the dispatch wall, behind a synchronous file appender.
    pub host_log_nanos: u64,
    pub host_logs: u64,
}

impl TurnSplit {
    fn add_assign(&mut self, other: Self) {
        self.recv_lag_nanos += other.recv_lag_nanos;
        self.recv_lags += other.recv_lags;
        self.dispatch_nanos += other.dispatch_nanos;
        self.dispatches += other.dispatches;
        self.turn_nanos += other.turn_nanos;
        self.turns += other.turns;
        self.export_lookup_nanos += other.export_lookup_nanos;
        self.export_lookups += other.export_lookups;
        self.guest_call_nanos += other.guest_call_nanos;
        self.guest_calls += other.guest_calls;
        self.span_nanos += other.span_nanos;
        self.spans += other.spans;
        self.host_log_nanos += other.host_log_nanos;
        self.host_logs += other.host_logs;
    }

    fn delta_since(self, before: Self) -> Self {
        Self {
            recv_lag_nanos: counter_delta(
                "recv_lag_nanos",
                self.recv_lag_nanos,
                before.recv_lag_nanos,
            ),
            recv_lags: counter_delta("recv_lags", self.recv_lags, before.recv_lags),
            dispatch_nanos: counter_delta(
                "dispatch_nanos",
                self.dispatch_nanos,
                before.dispatch_nanos,
            ),
            dispatches: counter_delta("dispatches", self.dispatches, before.dispatches),
            turn_nanos: counter_delta("turn_nanos", self.turn_nanos, before.turn_nanos),
            turns: counter_delta("turns", self.turns, before.turns),
            export_lookup_nanos: counter_delta(
                "export_lookup_nanos",
                self.export_lookup_nanos,
                before.export_lookup_nanos,
            ),
            export_lookups: counter_delta(
                "export_lookups",
                self.export_lookups,
                before.export_lookups,
            ),
            guest_call_nanos: counter_delta(
                "guest_call_nanos",
                self.guest_call_nanos,
                before.guest_call_nanos,
            ),
            guest_calls: counter_delta("guest_calls", self.guest_calls, before.guest_calls),
            span_nanos: counter_delta("span_nanos", self.span_nanos, before.span_nanos),
            spans: counter_delta("spans", self.spans, before.spans),
            host_log_nanos: counter_delta(
                "host_log_nanos",
                self.host_log_nanos,
                before.host_log_nanos,
            ),
            host_logs: counter_delta("host_logs", self.host_logs, before.host_logs),
        }
    }
}

/// One role's share of the cell-scope pulls: how many, and the chunks moved.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RolePulls {
    pub pulls: u64,
    pub chunks: u64,
}

impl RolePulls {
    fn delta_since(self, before: Self) -> Self {
        Self {
            pulls: counter_delta("role_pulls", self.pulls, before.pulls),
            chunks: counter_delta("role_pull_chunks", self.chunks, before.chunks),
        }
    }
}

/// One role's share of the served cell-scope peeks: how many, and the rows
/// they answered with.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RolePeeks {
    pub peeks: u64,
    pub rows: u64,
}

impl RolePeeks {
    fn delta_since(self, before: Self) -> Self {
        Self {
            peeks: counter_delta("role_peeks", self.peeks, before.peeks),
            rows: counter_delta("role_peek_rows", self.rows, before.rows),
        }
    }
}

/// What one namespace class's holders handed out to pulls: pages served,
/// chunks in them, how long those chunks had sat on the serving node, and how
/// many pages hit the size cap (a `next` cursor was returned).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ServedPulls {
    pub pulls: u64,
    pub chunks: u64,
    pub age_nanos: u64,
    pub age_skewed: u64,
    pub pages_full: u64,
}

impl ServedPulls {
    fn delta_since(self, before: Self) -> Self {
        Self {
            pulls: counter_delta("served_pulls", self.pulls, before.pulls),
            chunks: counter_delta("served_chunks", self.chunks, before.chunks),
            age_nanos: counter_delta("served_age_nanos", self.age_nanos, before.age_nanos),
            age_skewed: counter_delta("served_age_skewed", self.age_skewed, before.age_skewed),
            pages_full: counter_delta("served_pages_full", self.pages_full, before.pages_full),
        }
    }
}

impl ReplicationMetrics {
    #[must_use]
    pub fn from_metrics(metrics: &[Metric]) -> Self {
        let mut out = Self::default();

        for metric in metrics {
            let Some(Data::Sum(sum)) = metric.data.as_ref() else {
                continue;
            };

            for dp in &sum.data_points {
                let Some(value) = counter_value(dp) else {
                    continue;
                };
                let msg = string_attr(&dp.attributes, "msg");
                let cells =
                    string_attr(&dp.attributes, "ns") == Some(cell_protocol::NAMESPACE_CELLS);
                let replica = string_attr(&dp.attributes, "role") == Some("replica");

                match (metric.name.as_str(), msg) {
                    (REPL_MSGS_SENT, Some("ANNOUNCE")) => out.announces_sent += value,
                    (REPL_MSGS_RECV, Some("ANNOUNCE")) => out.announces_recv += value,
                    (REPL_MSGS_SENT, Some("CHANGESET")) => out.changesets_sent += value,
                    (REPL_MSGS_RECV, Some("CHANGESET")) => out.changesets_recv += value,
                    (REPL_ANNOUNCE_HEADS, _) => out.announce_heads += value,
                    (REPL_ANNOUNCE_BASELINES, _) => out.announce_baselines += value,
                    (REPL_ANNOUNCE_SCOPES, _) => out.announce_scopes += value,
                    (REPL_HANDLE_QUEUE_NANOS, Some("ANNOUNCE")) => {
                        out.announce_queue_nanos += value;
                    }
                    (REPL_HANDLE_NANOS, Some("ANNOUNCE")) => out.announce_nanos += value,
                    (REPL_HANDLED, Some("ANNOUNCE")) => out.announces_handled += value,
                    (REPL_APPLIED, _) => out.applied += value,
                    (REPL_APPLIED_AGE_NANOS, _) => out.applied_age_nanos += value,
                    (REPL_APPLIED_AGE_SKEWED, _) => out.applied_age_skewed += value,
                    (REPL_PULLS, _) => {
                        out.pulls += value;
                        if cells {
                            out.role_pulls_mut(replica).pulls += value;
                        }
                    }
                    (REPL_PULL_NANOS, _) => out.pull_nanos += value,
                    (REPL_PULL_CHUNKS, _) => {
                        out.pull_chunks += value;
                        if cells {
                            out.role_pulls_mut(replica).chunks += value;
                        }
                    }
                    (DB_PEEKS_SERVED, _) if cells => out.role_peeks_mut(replica).peeks += value,
                    (DB_PEEK_ROWS_SERVED, _) if cells => out.role_peeks_mut(replica).rows += value,
                    (REPL_SERVED_PULLS, _) => out.served_mut(cells).pulls += value,
                    (REPL_SERVED_CHUNKS, _) => out.served_mut(cells).chunks += value,
                    (REPL_SERVED_AGE_NANOS, _) => out.served_mut(cells).age_nanos += value,
                    (REPL_SERVED_AGE_SKEWED, _) => out.served_mut(cells).age_skewed += value,
                    (REPL_SERVED_PAGES_FULL, _) => out.served_mut(cells).pages_full += value,
                    _ => {}
                }
            }
        }

        out
    }

    fn served_mut(&mut self, cells: bool) -> &mut ServedPulls {
        if cells {
            &mut self.served_cells
        } else {
            &mut self.served_other
        }
    }

    fn role_pulls_mut(&mut self, replica: bool) -> &mut RolePulls {
        if replica {
            &mut self.pulls_replica_cells
        } else {
            &mut self.pulls_offload_cells
        }
    }

    fn role_peeks_mut(&mut self, replica: bool) -> &mut RolePeeks {
        if replica {
            &mut self.peeks_replica_cells
        } else {
            &mut self.peeks_other_cells
        }
    }

    /// Counters are cumulative per process, so a pass's own volume is the
    /// difference across it.
    #[must_use]
    pub fn delta_since(self, before: Self) -> Self {
        Self {
            announces_sent: counter_delta(
                "announces_sent",
                self.announces_sent,
                before.announces_sent,
            ),
            announces_recv: counter_delta(
                "announces_recv",
                self.announces_recv,
                before.announces_recv,
            ),
            changesets_sent: counter_delta(
                "changesets_sent",
                self.changesets_sent,
                before.changesets_sent,
            ),
            changesets_recv: counter_delta(
                "changesets_recv",
                self.changesets_recv,
                before.changesets_recv,
            ),
            announce_scopes: counter_delta(
                "announce_scopes",
                self.announce_scopes,
                before.announce_scopes,
            ),
            announce_heads: counter_delta(
                "announce_heads",
                self.announce_heads,
                before.announce_heads,
            ),
            announce_baselines: counter_delta(
                "announce_baselines",
                self.announce_baselines,
                before.announce_baselines,
            ),
            announce_queue_nanos: counter_delta(
                "announce_queue_nanos",
                self.announce_queue_nanos,
                before.announce_queue_nanos,
            ),
            announce_nanos: counter_delta(
                "announce_nanos",
                self.announce_nanos,
                before.announce_nanos,
            ),
            announces_handled: counter_delta(
                "announces_handled",
                self.announces_handled,
                before.announces_handled,
            ),
            applied: counter_delta("applied", self.applied, before.applied),
            applied_age_nanos: counter_delta(
                "applied_age_nanos",
                self.applied_age_nanos,
                before.applied_age_nanos,
            ),
            applied_age_skewed: counter_delta(
                "applied_age_skewed",
                self.applied_age_skewed,
                before.applied_age_skewed,
            ),
            pulls: counter_delta("pulls", self.pulls, before.pulls),
            pull_nanos: counter_delta("pull_nanos", self.pull_nanos, before.pull_nanos),
            pull_chunks: counter_delta("pull_chunks", self.pull_chunks, before.pull_chunks),
            served_cells: self.served_cells.delta_since(before.served_cells),
            served_other: self.served_other.delta_since(before.served_other),
            pulls_replica_cells: self
                .pulls_replica_cells
                .delta_since(before.pulls_replica_cells),
            pulls_offload_cells: self
                .pulls_offload_cells
                .delta_since(before.pulls_offload_cells),
            peeks_replica_cells: self
                .peeks_replica_cells
                .delta_since(before.peeks_replica_cells),
            peeks_other_cells: self.peeks_other_cells.delta_since(before.peeks_other_cells),
        }
    }
}

/// Every distinct event name/topic seen on `cell_events_sent`'s `event` attribute (see
/// `cell-mailbox`'s `OutgoingMessage::send_via_db`, which tags this counter with the event's own
/// name at send time) — the DB's own event tables are scoped per event name, not per cell (see
/// `cell_protocol::scope_of_event`: every publisher of a given event name shares one table), so
/// this is how a benchmark-agnostic caller discovers which topics exist to query, rather than
/// hardcoding a scenario's event names.
#[must_use]
pub fn event_names(metrics: &[Metric]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for metric in metrics {
        if metric.name != CELL_EVENTS_SENT {
            continue;
        }
        let Some(Data::Sum(sum)) = metric.data.as_ref() else {
            continue;
        };
        for dp in &sum.data_points {
            if let Some(event) = string_attr(&dp.attributes, "event") {
                names.insert(event.to_owned());
            }
        }
    }
    names
}

impl CellInteractionMetricsSnapshot {
    #[must_use]
    pub fn from_metrics(metrics: &[Metric]) -> Self {
        let mut snapshot = Self::default();
        for metric in metrics {
            let Some(data) = metric.data.as_ref() else {
                continue;
            };
            let Data::Sum(sum) = data else {
                continue;
            };

            for dp in &sum.data_points {
                snapshot.insert_data_point(&metric.name, dp);
            }
        }
        snapshot
    }

    #[must_use]
    pub fn delta_since(&self, before: &Self) -> Self {
        let mut cells = BTreeMap::new();
        let sris = self
            .cells
            .keys()
            .chain(before.cells.keys())
            .collect::<BTreeSet<_>>();

        for sri in sris {
            let after = self.cells.get(sri).copied().unwrap_or_default();
            let before = before.cells.get(sri).copied().unwrap_or_default();
            cells.insert(*sri, after.delta_since(before));
        }
        Self { cells }
    }

    #[must_use]
    pub fn totals(&self) -> CellInteractionMetrics {
        let mut totals = CellInteractionMetrics::default();
        for metrics in self.cells.values() {
            totals.add_assign(*metrics);
        }
        totals
    }

    /// Sum all counters for cells whose SRI is in `sris`.
    ///
    /// Pass every replica's SRI (e.g. all zone shards) to aggregate across a whole tier.
    #[must_use]
    pub fn matching_sri(&self, sris: &[Sri]) -> CellInteractionMetrics {
        let mut totals = CellInteractionMetrics::default();
        for (sri, metrics) in &self.cells {
            if sris.contains(sri) {
                totals.add_assign(*metrics);
            }
        }
        totals
    }

    /// Compute how many commands/events sent were never received by a cell during this delta.
    ///
    /// `externally_injected_commands` and `externally_injected_events` account for messages that
    /// entered the system from outside a cell, such as the benchmark harness sending its initial
    /// command.
    ///
    /// Unlike [`Self::assert_no_loss`], this never panics — use it in contexts (like a benchmark
    /// under load) where loss is an expected, measurable outcome rather than a harness bug.
    #[must_use]
    pub fn loss(
        &self,
        externally_injected_commands: u64,
        externally_injected_events: u64,
    ) -> LossReport {
        let totals = self.totals();
        LossReport {
            commands_lost: (totals.commands_sent + externally_injected_commands)
                .saturating_sub(totals.commands_received),
            events_lost: (totals.events_sent + externally_injected_events)
                .saturating_sub(totals.events_received),
        }
    }

    /// Assert that every command/event sent by a cell was received by a cell during this delta.
    ///
    /// Panics on any loss. Use in tests that expect a lossless run; benchmarks that expect loss
    /// under load should use [`Self::loss`] and report it instead.
    pub fn assert_no_loss(
        &self,
        externally_injected_commands: u64,
        externally_injected_events: u64,
    ) {
        let loss = self.loss(externally_injected_commands, externally_injected_events);
        assert_eq!(loss.commands_lost, 0, "command loss detected:\n{self}");
        assert_eq!(loss.events_lost, 0, "event loss detected:\n{self}");
    }

    fn insert_data_point(&mut self, name: &str, dp: &NumberDataPoint) {
        let Some(sri) = string_attr(&dp.attributes, "sri") else {
            return;
        };
        let Some(value) = counter_value(dp) else {
            return;
        };

        let Ok(sri) = sri.parse::<Sri>() else {
            return;
        };
        let entry = self.cells.entry(sri).or_default();
        match name {
            CELL_COMMANDS_PROCESSED => entry.commands_received += value,
            CELL_COMMANDS_SENT => entry.commands_sent += value,
            CELL_COMMANDS_FAILED => entry.commands_failed += value,
            CELL_MAILBOX_POLLS => entry.mailbox_polls += value,
            CELL_MAILBOX_EMPTY_POLLS => entry.mailbox_empty_polls += value,
            CELL_MAILBOX_BACKSTOP_POLLS => entry.mailbox_backstop_polls += value,
            CELL_MAILBOX_BACKSTOP_EMPTY_POLLS => entry.mailbox_backstop_empty_polls += value,
            CELL_MAILBOX_NOTIFICATIONS => entry.mailbox_notifications += value,
            CELL_COMMANDS_DELIVERED_POKE => entry.commands_delivered_poke += value,
            CELL_COMMANDS_DELIVERED_BACKSTOP => entry.commands_delivered_backstop += value,
            CELL_COMMIT_NANOS => entry.commit_nanos += value,
            CELL_COMMITS => entry.commits += value,
            CELL_PEEK_NANOS => entry.peek_nanos += value,
            CELL_PEEKS => entry.peeks += value,
            CELL_WAIT_NANOS => entry.wait_nanos += value,
            CELL_WAITS => entry.waits += value,
            CELL_BACKSTOP_WAIT_NANOS => entry.backstop_wait_nanos += value,
            CELL_BACKSTOP_WAITS => entry.backstop_waits += value,
            CELL_READ_FAILURES => entry.read_failures += value,
            CELL_MAILBOX_DEPTH_SUM => entry.mailbox_depth_sum += value,
            CELL_MAILBOX_DEPTH_SAMPLES => entry.mailbox_depth_samples += value,
            CELL_RECV_LAG_NANOS => entry.turn.recv_lag_nanos += value,
            CELL_RECV_LAGS => entry.turn.recv_lags += value,
            CELL_DISPATCH_NANOS => entry.turn.dispatch_nanos += value,
            CELL_DISPATCHES => entry.turn.dispatches += value,
            CELL_TURN_NANOS => entry.turn.turn_nanos += value,
            CELL_TURNS => entry.turn.turns += value,
            CELL_EXPORT_LOOKUP_NANOS => entry.turn.export_lookup_nanos += value,
            CELL_EXPORT_LOOKUPS => entry.turn.export_lookups += value,
            CELL_GUEST_CALL_NANOS => entry.turn.guest_call_nanos += value,
            CELL_GUEST_CALLS => entry.turn.guest_calls += value,
            CELL_SPAN_NANOS => entry.turn.span_nanos += value,
            CELL_SPANS => entry.turn.spans += value,
            CELL_HOST_LOG_NANOS => entry.turn.host_log_nanos += value,
            CELL_HOST_LOGS => entry.turn.host_logs += value,
            CELL_EVENTS_PROCESSED => entry.events_received += value,
            CELL_EVENTS_SENT => entry.events_sent += value,
            _ => {}
        }
    }
}

impl CellInteractionMetrics {
    fn add_assign(&mut self, other: Self) {
        self.commands_received += other.commands_received;
        self.commands_sent += other.commands_sent;
        self.commands_failed += other.commands_failed;
        self.mailbox_polls += other.mailbox_polls;
        self.mailbox_empty_polls += other.mailbox_empty_polls;
        self.mailbox_backstop_polls += other.mailbox_backstop_polls;
        self.mailbox_backstop_empty_polls += other.mailbox_backstop_empty_polls;
        self.mailbox_notifications += other.mailbox_notifications;
        self.commands_delivered_poke += other.commands_delivered_poke;
        self.commands_delivered_backstop += other.commands_delivered_backstop;
        self.commit_nanos += other.commit_nanos;
        self.commits += other.commits;
        self.peek_nanos += other.peek_nanos;
        self.peeks += other.peeks;
        self.wait_nanos += other.wait_nanos;
        self.waits += other.waits;
        self.backstop_wait_nanos += other.backstop_wait_nanos;
        self.backstop_waits += other.backstop_waits;
        self.read_failures += other.read_failures;
        self.mailbox_depth_sum += other.mailbox_depth_sum;
        self.mailbox_depth_samples += other.mailbox_depth_samples;
        self.turn.add_assign(other.turn);
        self.events_received += other.events_received;
        self.events_sent += other.events_sent;
    }

    fn delta_since(self, before: Self) -> Self {
        Self {
            commands_received: counter_delta(
                "commands_received",
                self.commands_received,
                before.commands_received,
            ),
            commands_sent: counter_delta("commands_sent", self.commands_sent, before.commands_sent),
            commands_failed: counter_delta(
                "commands_failed",
                self.commands_failed,
                before.commands_failed,
            ),
            mailbox_polls: counter_delta("mailbox_polls", self.mailbox_polls, before.mailbox_polls),
            mailbox_empty_polls: counter_delta(
                "mailbox_empty_polls",
                self.mailbox_empty_polls,
                before.mailbox_empty_polls,
            ),
            mailbox_backstop_polls: counter_delta(
                "mailbox_backstop_polls",
                self.mailbox_backstop_polls,
                before.mailbox_backstop_polls,
            ),
            mailbox_backstop_empty_polls: counter_delta(
                "mailbox_backstop_empty_polls",
                self.mailbox_backstop_empty_polls,
                before.mailbox_backstop_empty_polls,
            ),
            mailbox_notifications: counter_delta(
                "mailbox_notifications",
                self.mailbox_notifications,
                before.mailbox_notifications,
            ),
            commands_delivered_poke: counter_delta(
                "commands_delivered_poke",
                self.commands_delivered_poke,
                before.commands_delivered_poke,
            ),
            commands_delivered_backstop: counter_delta(
                "commands_delivered_backstop",
                self.commands_delivered_backstop,
                before.commands_delivered_backstop,
            ),
            commit_nanos: counter_delta("commit_nanos", self.commit_nanos, before.commit_nanos),
            commits: counter_delta("commits", self.commits, before.commits),
            peek_nanos: counter_delta("peek_nanos", self.peek_nanos, before.peek_nanos),
            peeks: counter_delta("peeks", self.peeks, before.peeks),
            wait_nanos: counter_delta("wait_nanos", self.wait_nanos, before.wait_nanos),
            waits: counter_delta("waits", self.waits, before.waits),
            backstop_wait_nanos: counter_delta(
                "backstop_wait_nanos",
                self.backstop_wait_nanos,
                before.backstop_wait_nanos,
            ),
            backstop_waits: counter_delta(
                "backstop_waits",
                self.backstop_waits,
                before.backstop_waits,
            ),
            read_failures: counter_delta("read_failures", self.read_failures, before.read_failures),
            mailbox_depth_sum: counter_delta(
                "mailbox_depth_sum",
                self.mailbox_depth_sum,
                before.mailbox_depth_sum,
            ),
            mailbox_depth_samples: counter_delta(
                "mailbox_depth_samples",
                self.mailbox_depth_samples,
                before.mailbox_depth_samples,
            ),
            turn: self.turn.delta_since(before.turn),
            events_received: counter_delta(
                "events_received",
                self.events_received,
                before.events_received,
            ),
            events_sent: counter_delta("events_sent", self.events_sent, before.events_sent),
        }
    }
}

fn counter_delta(name: &str, after: u64, before: u64) -> u64 {
    after.checked_sub(before).unwrap_or_else(|| {
        panic!("metric counter {name} went backwards: before={before}, after={after}")
    })
}

fn counter_value(dp: &NumberDataPoint) -> Option<u64> {
    match dp.value? {
        number_data_point::Value::AsInt(value) => value.try_into().ok(),
        number_data_point::Value::AsDouble(_) => None,
    }
}

fn string_attr<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attr| attr.key == key)
        .and_then(|attr| attr.value.as_ref())
        .and_then(|value| value.value.as_ref())
        .and_then(|value| match value {
            Value::StringValue(value) => Some(value.as_str()),
            _ => None,
        })
}

impl Display for CellInteractionMetricsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let totals = self.totals();
        writeln!(
            f,
            "total: commands rx={}, tx={}; events rx={}, tx={}",
            totals.commands_received,
            totals.commands_sent,
            totals.events_received,
            totals.events_sent
        )?;
        for (sri, metrics) in &self.cells {
            writeln!(
                f,
                "  {sri}: commands rx={}, tx={}; events rx={}, tx={}",
                metrics.commands_received,
                metrics.commands_sent,
                metrics.events_received,
                metrics.events_sent
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use swarm_telemetry::db::opentelemetry_proto::tonic::{
        common::v1::{AnyValue, KeyValue},
        metrics::v1::{Metric, Sum},
    };

    use super::*;

    fn metric(name: &str, points: &[(&str, i64)]) -> Metric {
        Metric {
            name: name.to_owned(),
            data: Some(Data::Sum(Sum {
                data_points: points
                    .iter()
                    .map(|(sri, value)| NumberDataPoint {
                        attributes: vec![KeyValue {
                            key: "sri".to_owned(),
                            value: Some(AnyValue {
                                value: Some(Value::StringValue((*sri).to_owned())),
                            }),
                            key_strindex: 0,
                        }],
                        value: Some(number_data_point::Value::AsInt(*value)),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    // Telemetry attaches a cell's SRI to metrics as its canonical (UUID) `Display` text (see
    // `insert_data_point`), never a raw human name — so tests feed derived SRI strings rather
    // than literals like "a"/"b".
    fn test_sri(name: &str) -> Sri {
        Sri::of_path(name).unwrap()
    }

    #[test]
    fn aggregates_cell_interaction_metrics() {
        let a = test_sri("a").to_string();
        let b = test_sri("b").to_string();
        let snapshot = CellInteractionMetricsSnapshot::from_metrics(&[
            metric(CELL_COMMANDS_PROCESSED, &[(a.as_str(), 2), (b.as_str(), 1)]),
            metric(CELL_COMMANDS_SENT, &[(a.as_str(), 1)]),
            metric(CELL_EVENTS_PROCESSED, &[(b.as_str(), 3)]),
            metric(CELL_EVENTS_SENT, &[(a.as_str(), 3)]),
        ]);

        assert_eq!(
            snapshot.totals(),
            CellInteractionMetrics {
                commands_received: 3,
                commands_sent: 1,
                events_received: 3,
                events_sent: 3,
                ..Default::default()
            }
        );
        assert_eq!(snapshot.cells[&test_sri("a")].commands_received, 2);
        assert_eq!(snapshot.cells[&test_sri("b")].events_received, 3);
    }

    #[test]
    fn computes_delta_across_cells() {
        let a = test_sri("a").to_string();
        let b = test_sri("b").to_string();
        let before = CellInteractionMetricsSnapshot::from_metrics(&[
            metric(CELL_COMMANDS_PROCESSED, &[(a.as_str(), 2)]),
            metric(CELL_COMMANDS_SENT, &[(a.as_str(), 1)]),
        ]);
        let after = CellInteractionMetricsSnapshot::from_metrics(&[
            metric(CELL_COMMANDS_PROCESSED, &[(a.as_str(), 4), (b.as_str(), 1)]),
            metric(CELL_COMMANDS_SENT, &[(a.as_str(), 3)]),
        ]);

        let delta = after.delta_since(&before);

        assert_eq!(delta.cells[&test_sri("a")].commands_received, 2);
        assert_eq!(delta.cells[&test_sri("a")].commands_sent, 2);
        assert_eq!(delta.cells[&test_sri("b")].commands_received, 1);
    }

    #[test]
    fn aggregates_by_matching_sri_set() {
        let obj0 = test_sri("asset.object.0").to_string();
        let obj1 = test_sri("asset.object.1").to_string();
        let zone0 = test_sri("agent.zone.0").to_string();
        let snapshot = CellInteractionMetricsSnapshot::from_metrics(&[
            metric(
                CELL_COMMANDS_PROCESSED,
                &[(obj0.as_str(), 2), (obj1.as_str(), 3), (zone0.as_str(), 4)],
            ),
            metric(CELL_EVENTS_SENT, &[(zone0.as_str(), 1)]),
        ]);

        assert_eq!(
            snapshot
                .matching_sri(&[test_sri("asset.object.0"), test_sri("asset.object.1")])
                .commands_received,
            5
        );
        assert_eq!(
            snapshot
                .matching_sri(&[test_sri("agent.zone.0")])
                .events_sent,
            1
        );
    }

    #[test]
    fn no_loss_allows_external_injection() {
        let asset = test_sri("asset").to_string();
        let zone = test_sri("zone").to_string();
        let central = test_sri("central").to_string();
        let delta = CellInteractionMetricsSnapshot::from_metrics(&[
            metric(
                CELL_COMMANDS_PROCESSED,
                &[(asset.as_str(), 1), (zone.as_str(), 1)],
            ),
            metric(CELL_COMMANDS_SENT, &[(asset.as_str(), 1)]),
            metric(CELL_EVENTS_PROCESSED, &[(central.as_str(), 1)]),
            metric(CELL_EVENTS_SENT, &[(zone.as_str(), 1)]),
        ]);

        delta.assert_no_loss(1, 0);
    }

    #[test]
    fn loss_reports_without_panicking() {
        let asset = test_sri("asset").to_string();
        let delta = CellInteractionMetricsSnapshot::from_metrics(&[
            metric(CELL_COMMANDS_SENT, &[(asset.as_str(), 3)]),
            metric(CELL_COMMANDS_PROCESSED, &[(asset.as_str(), 1)]),
        ]);

        let loss = delta.loss(0, 0);
        assert_eq!(loss.commands_lost, 2);
        assert_eq!(loss.events_lost, 0);
        assert!(loss.any());
    }
}
