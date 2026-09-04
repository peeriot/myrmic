//! Replication wire metrics, recorded at the transport boundary.
//!
//! The db plugin had no instrumentation at all, which left the thing that
//! actually moves cell-to-cell messages unmeasured. A cell's outgoing append is
//! deferred into a transaction routed by the *sender's* scope, so it lands on
//! the sender's holder and reaches the recipient only once replication drains
//! it — and run 33430437208 put that drain at **1.6s** at load 1000 while the
//! recipient sat 94% idle.
//!
//! These say where inside the drain cycle it goes. A row moves by announce ->
//! pull -> apply, so few, fat pulls mean rows are waiting to be *told about*
//! (the announce cadence, or a dropped one falling back to the periodic
//! `2s + rand(100..6000)ms`), while many, slow pulls mean the transfer itself
//! is the cost.
//!
//! Node-level, so nothing here carries an `sri` — see
//! `test_framework::metrics::ReplicationMetrics`, which aggregates them across
//! the mesh rather than per cell.

use std::sync::LazyLock;

use db_commons::models;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;

struct ReplicationMetrics {
    msgs_sent: Counter<u64>,
    msgs_recv: Counter<u64>,
    announce_heads: Counter<u64>,
    announce_baselines: Counter<u64>,
    announce_scopes: Counter<u64>,
    handle_queue_nanos: Counter<u64>,
    handle_nanos: Counter<u64>,
    handled: Counter<u64>,
    applied: Counter<u64>,
    applied_age_nanos: Counter<u64>,
    applied_age_skewed: Counter<u64>,
    pulls: Counter<u64>,
    pull_nanos: Counter<u64>,
    pull_chunks: Counter<u64>,
    served_pulls: Counter<u64>,
    served_chunks: Counter<u64>,
    served_age_nanos: Counter<u64>,
    served_age_skewed: Counter<u64>,
    served_pages_full: Counter<u64>,
    peeks_served: Counter<u64>,
    peek_rows_served: Counter<u64>,
}

static METRICS: LazyLock<ReplicationMetrics> = LazyLock::new(|| {
    let meter = opentelemetry::global::meter("db_replication");
    ReplicationMetrics {
        msgs_sent: meter.u64_counter("repl_msgs_sent").build(),
        msgs_recv: meter.u64_counter("repl_msgs_recv").build(),
        announce_heads: meter.u64_counter("repl_announce_heads").build(),
        announce_baselines: meter.u64_counter("repl_announce_baselines").build(),
        announce_scopes: meter.u64_counter("repl_announce_scopes").build(),
        handle_queue_nanos: meter.u64_counter("repl_handle_queue_nanos").build(),
        handle_nanos: meter.u64_counter("repl_handle_nanos").build(),
        handled: meter.u64_counter("repl_handled").build(),
        applied: meter.u64_counter("repl_applied").build(),
        applied_age_nanos: meter.u64_counter("repl_applied_age_nanos").build(),
        applied_age_skewed: meter.u64_counter("repl_applied_age_skewed").build(),
        pulls: meter.u64_counter("repl_pulls").build(),
        pull_nanos: meter.u64_counter("repl_pull_nanos").build(),
        pull_chunks: meter.u64_counter("repl_pull_chunks").build(),
        served_pulls: meter.u64_counter("repl_served_pulls").build(),
        served_chunks: meter.u64_counter("repl_served_chunks").build(),
        served_age_nanos: meter.u64_counter("repl_served_age_nanos").build(),
        served_age_skewed: meter.u64_counter("repl_served_age_skewed").build(),
        served_pages_full: meter.u64_counter("repl_served_pages_full").build(),
        peeks_served: meter.u64_counter("db_peeks_served").build(),
        peek_rows_served: meter.u64_counter("db_peek_rows_served").build(),
    }
});

fn pid() -> KeyValue {
    KeyValue::new("pid", std::process::id().to_string())
}

/// One replica message published. `kind` is [`ReplicaMessage::name`], so
/// announces can be told apart from changesets — they are separate zenoh
/// pushes, and both default to `CongestionControl::Drop`.
///
/// [`ReplicaMessage::name`]: db_commons::models::ReplicaMessage::name
pub(crate) fn record_msg_sent(kind: &'static str) {
    METRICS
        .msgs_sent
        .add(1, &[KeyValue::new("msg", kind), pid()]);
}

/// One replica message received. An announce is broadcast to every replicating
/// node, so this is not `msgs_sent` times one — read the two as volumes, not as
/// a delivery ratio.
pub(crate) fn record_msg_recv(kind: &'static str) {
    METRICS
        .msgs_recv
        .add(1, &[KeyValue::new("msg", kind), pid()]);
}

/// The shape of one announce as published: how many scopes it covers, how many
/// explicit heads it carries, and how many of those scopes managed to elide
/// anything behind a baseline.
///
/// Heads per announce is the number being chased. `plan_catchup` iterates every
/// head of every scope on *every* receipt, and there were 40833 receipts to move
/// 5979 rows at load 1000 — so if heads grow through a pass, the cost of an
/// announce grows with them, quadratically in aggregate. `ANNOUNCE_LAG` keeps
/// heads younger than 30s explicit, and a pass is about 20s, which would mean
/// nothing is ever elided and every announce carries every commit so far.
/// Baselines against scopes says whether that is what happens.
pub(crate) fn record_announce(scopes: usize, heads: usize, baselines: usize) {
    let attrs = [pid()];
    METRICS.announce_scopes.add(scopes as u64, &attrs);
    METRICS.announce_heads.add(heads as u64, &attrs);
    METRICS.announce_baselines.add(baselines as u64, &attrs);
}

/// One received message's handling, split at the point that matters: `queued` is
/// how long the spawned task waited before it ran at all, `ran` is how long it
/// then took.
///
/// The split is the whole question. Announce *work* is small — 5.3 million head
/// comparisons plus postcard decoding at load 1000 comes to well under a second
/// of CPU spread over sixteen nodes, nowhere near the 1.6s a row waits. Announce
/// *volume* is not: 41007 receipts, each a task spawned onto a node's single
/// tokio runtime alongside data-plane work that runs its store operations
/// synchronously inline. If the wait is in `queued` rather than `ran`, the cost
/// of an announce is that it takes a turn, not that it does anything, and the
/// fix is scheduling or fewer of them rather than a cheaper diff.
pub(crate) fn record_handled(
    kind: &'static str,
    queued: std::time::Duration,
    ran: std::time::Duration,
) {
    let attrs = [KeyValue::new("msg", kind), pid()];
    let nanos = |d: std::time::Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);

    METRICS.handled.add(1, &attrs);
    METRICS.handle_queue_nanos.add(nanos(queued), &attrs);
    METRICS.handle_nanos.add(nanos(ran), &attrs);
}

/// How old one pulled chunk was by the time it arrived — the gap between the
/// version the writer stamped it with and this node's clock now — plus how often
/// that gap came out negative.
///
/// The skew count is the point. This compares two nodes' HLC readings, so a
/// receiver whose clock trails the writer's produces a negative age; clamping
/// that to zero drags the mean down and can make a slow arrival look instant.
/// The mean this reported (23ms at load 1000) is what sent the investigation to
/// the read side, and the proof of concept built on it changed nothing — so
/// before trusting the mean again, count how much of it is clamped.
pub(crate) fn record_applied_age(now: models::Version, version: models::Version) {
    let attrs = [pid()];
    METRICS.applied.add(1, &attrs);

    if version >= now {
        METRICS.applied_age_skewed.add(1, &attrs);
        return;
    }

    let age = uhlc::NTP64(now - version).to_duration();
    METRICS
        .applied_age_nanos
        .add(u64::try_from(age.as_nanos()).unwrap_or(u64::MAX), &attrs);
}

/// One served pull page: how long each chunk sat on *this* node between being
/// stamped and being served out, split by scope namespace, plus whether the
/// page hit the size cap.
///
/// This is the single-clock counterpart of [`record_applied_age`], and the one
/// to trust when they disagree. A cell's transaction commits on whatever node
/// it lands on, so the chunks a holder serves for `CELLS` scopes carry
/// versions stamped by this node's own clock — `now - version` here is an
/// honest wait. The receiver-side age is not: a receiver's HLC is
/// max(physical clock, every stamp it has applied), so a receiver whose
/// physical clock trails reports roughly the stream's inter-commit spacing
/// however long the rows actually waited in transit. A serve-side mean in
/// seconds against a receiver-side mean in milliseconds means rows strand
/// here waiting to be pulled, and the receiver number is the artefact.
pub(crate) fn record_pull_served<I>(namespace: &str, now: models::Version, versions: I, more: bool)
where
    I: IntoIterator<Item = models::Version>,
{
    let attrs = [KeyValue::new("ns", namespace.to_owned()), pid()];

    METRICS.served_pulls.add(1, &attrs);
    if more {
        METRICS.served_pages_full.add(1, &attrs);
    }

    for version in versions {
        METRICS.served_chunks.add(1, &attrs);

        if version >= now {
            METRICS.served_age_skewed.add(1, &attrs);
            continue;
        }

        let age = uhlc::NTP64(now - version).to_duration();
        METRICS
            .served_age_nanos
            .add(u64::try_from(age.as_nanos()).unwrap_or(u64::MAX), &attrs);
    }
}

/// One direct pull round trip: how long it took and how many chunks came back,
/// tagged with the puller's role and the scope's namespace. A chunk is one
/// sync point, so one commit's worth of rows.
///
/// The role is the open question the totals cannot answer: rows leave their
/// writer within ~20ms of commit (`record_pull_served`) yet take seconds to
/// reach the reader, so *who* is doing that prompt pulling — the scope's
/// locate-visible replica, or a fellow offloader the reader can never be
/// routed to — decides whether a pulled row is a delivered row.
pub(crate) fn record_pull(
    role: &'static str,
    namespace: &str,
    elapsed: std::time::Duration,
    chunks: usize,
) {
    let attrs = [
        KeyValue::new("role", role),
        KeyValue::new("ns", namespace.to_owned()),
        pid(),
    ];
    METRICS.pulls.add(1, &attrs);
    METRICS.pull_nanos.add(
        u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
        &attrs,
    );
    METRICS.pull_chunks.add(chunks as u64, &attrs);
}

/// One served `tb_peek`: whether this node is a locate-visible replica of the
/// scope or something else (an offloader, or a node the read fell back to),
/// and how many rows it answered with.
///
/// The reader-side depth samples say the peek target's table is near empty
/// while a command takes seconds to arrive, so the question is not what the
/// serving node holds but *which class of node keeps being asked*: peeks
/// landing on non-replicas mean the read ranking routes readers to nodes that
/// only ever hold their own newest slice.
pub(crate) fn record_peek_served(namespace: &str, replica: bool, rows: usize) {
    let attrs = [
        KeyValue::new("role", if replica { "replica" } else { "other" }),
        KeyValue::new("ns", namespace.to_owned()),
        pid(),
    ];
    METRICS.peeks_served.add(1, &attrs);
    METRICS.peek_rows_served.add(rows as u64, &attrs);
}
