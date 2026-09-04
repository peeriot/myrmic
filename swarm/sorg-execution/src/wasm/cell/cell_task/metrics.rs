//! Emits `cell_commands_processed`/`cell_events_processed` once a message has
//! actually run all the way through the cell's single message loop (handler
//! executed, state transaction committed) — as opposed to
//! `cell_commands_queued`/`cell_events_queued` (see
//! `cell_mailbox::command::CommandMetrics` and
//! `crate::wasm::cell::state::message_handler::event_listener`), which fire as
//! soon as a message is pulled off its mailbox/subscription and handed to the
//! loop, before it's actually dispatched. Conflating the two under the name
//! "processed" hid the loop's queueing delay under saturation: the queued
//! counter alone made it look like every message was handled the instant it
//! arrived.

use std::sync::LazyLock;

use cell_protocol::Sri;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;

struct ProcessedMetrics {
    commands: Counter<u64>,
    events: Counter<u64>,
    commands_failed: Counter<u64>,
    commit_nanos: Counter<u64>,
    commits: Counter<u64>,
    recv_lag_nanos: Counter<u64>,
    recv_lags: Counter<u64>,
    dispatch_nanos: Counter<u64>,
    dispatches: Counter<u64>,
    turn_nanos: Counter<u64>,
    turns: Counter<u64>,
    export_lookup_nanos: Counter<u64>,
    export_lookups: Counter<u64>,
    guest_call_nanos: Counter<u64>,
    guest_calls: Counter<u64>,
    span_nanos: Counter<u64>,
    spans: Counter<u64>,
    host_log_nanos: Counter<u64>,
    host_logs: Counter<u64>,
}

static METRICS: LazyLock<ProcessedMetrics> = LazyLock::new(|| {
    let meter = opentelemetry::global::meter("cell_interaction");
    ProcessedMetrics {
        commands: meter.u64_counter("cell_commands_processed").build(),
        events: meter.u64_counter("cell_events_processed").build(),
        commands_failed: meter.u64_counter("cell_commands_failed").build(),
        commit_nanos: meter.u64_counter("cell_commit_nanos").build(),
        commits: meter.u64_counter("cell_commits").build(),
        recv_lag_nanos: meter.u64_counter("cell_recv_lag_nanos").build(),
        recv_lags: meter.u64_counter("cell_recv_lags").build(),
        dispatch_nanos: meter.u64_counter("cell_dispatch_nanos").build(),
        dispatches: meter.u64_counter("cell_dispatches").build(),
        turn_nanos: meter.u64_counter("cell_turn_nanos").build(),
        turns: meter.u64_counter("cell_turns").build(),
        export_lookup_nanos: meter.u64_counter("cell_export_lookup_nanos").build(),
        export_lookups: meter.u64_counter("cell_export_lookups").build(),
        guest_call_nanos: meter.u64_counter("cell_guest_call_nanos").build(),
        guest_calls: meter.u64_counter("cell_guest_calls").build(),
        span_nanos: meter.u64_counter("cell_span_nanos").build(),
        spans: meter.u64_counter("cell_spans").build(),
        host_log_nanos: meter.u64_counter("cell_host_log_nanos").build(),
        host_logs: meter.u64_counter("cell_host_logs").build(),
    }
});

fn attrs_of(sri: &Sri) -> [KeyValue; 2] {
    [
        KeyValue::new("sri", sri.to_string()),
        KeyValue::new("pid", std::process::id().to_string()),
    ]
}

fn nanos_of(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// The three timers that split one command's turn through the cell task, for
/// finding time no other counter accounts for. Dispatches at load 1000 are
/// strict singles ~10ms apart while the handler runs 13µs, the commit 0.4ms
/// and the mailbox sits mid-batch (6 rows a peek) — so ~5-10ms per command
/// hides between the producer handing a command over and the loop finishing
/// it. `recv_lag` is the mpsc hop (producer send → loop receive: run-queue
/// wait of the cell task's wake), `dispatch` is the handler call wall-clock
/// (which, unlike the handler span, includes every fuel-yield suspension),
/// and `turn` is the producer's whole send → handled round trip; turn minus
/// its parts is what remains unexplained.
pub(crate) fn record_turn_split(
    sri: &Sri,
    recv_lag: Option<std::time::Duration>,
    dispatch: std::time::Duration,
) {
    let attrs = [
        KeyValue::new("sri", sri.to_string()),
        KeyValue::new("pid", std::process::id().to_string()),
    ];
    let nanos = |d: std::time::Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);

    if let Some(recv_lag) = recv_lag {
        METRICS.recv_lag_nanos.add(nanos(recv_lag), &attrs);
        METRICS.recv_lags.add(1, &attrs);
    }
    METRICS.dispatch_nanos.add(nanos(dispatch), &attrs);
    METRICS.dispatches.add(1, &attrs);
}

/// One producer-side send → handled round trip — see [`record_turn_split`].
pub(crate) fn record_turn(sri: &Sri, turn: std::time::Duration) {
    let attrs = [
        KeyValue::new("sri", sri.to_string()),
        KeyValue::new("pid", std::process::id().to_string()),
    ];
    METRICS
        .turn_nanos
        .add(u64::try_from(turn.as_nanos()).unwrap_or(u64::MAX), &attrs);
    METRICS.turns.add(1, &attrs);
}

/// Splits the dispatch wall itself, once the routed placement read is out of it
/// (`e26fc85e`) and dispatch is down to ~411µs around a 13µs handler.
/// `lookup` is `Instance::get_typed_func` — a name lookup and signature check
/// against the instance's exports, paid per command — and `guest` is the
/// `call_async` wall, which is fiber entry plus everything the guest does,
/// including its host calls. Their sum against the dispatch total says whether
/// the residual is the call machinery or something before it.
pub(crate) fn record_dispatch_split(
    sri: &Sri,
    lookup: std::time::Duration,
    guest: std::time::Duration,
) {
    let attrs = attrs_of(sri);
    METRICS.export_lookup_nanos.add(nanos_of(lookup), &attrs);
    METRICS.export_lookups.add(1, &attrs);
    METRICS.guest_call_nanos.add(nanos_of(guest), &attrs);
    METRICS.guest_calls.add(1, &attrs);
}

/// The observability span's own cost across one turn: creation, remote-parent
/// linking and the brief enter that starts it, plus the drop that ends it. It
/// sits outside the dispatch wall but inside the turn, so it is part of what
/// turn-minus-its-parts was hiding.
pub(crate) fn record_span(sri: &Sri, elapsed: std::time::Duration) {
    let attrs = attrs_of(sri);
    METRICS.span_nanos.add(nanos_of(elapsed), &attrs);
    METRICS.spans.add(1, &attrs);
}

/// One `log` host call, guest entry to return. Every benchmark handler makes
/// exactly one, and the rolling file appender behind it is synchronous, so this
/// is the guest's own share of the dispatch wall.
pub(crate) fn record_host_log(sri: &Sri, elapsed: std::time::Duration) {
    let attrs = attrs_of(sri);
    METRICS.host_log_nanos.add(nanos_of(elapsed), &attrs);
    METRICS.host_logs.add(1, &attrs);
}

/// Records that `sri` finished processing a command named `cmd`.
///
/// Successes only. A failed handler rolls its transaction back, which discards
/// the deferred mailbox delete by design, so the command is redelivered and
/// tried again — counting those attempts here made
/// `commands_sent.saturating_sub(commands_received)` clamp to zero whenever a
/// command was retried at all, reporting a lossless run by construction. They
/// go to [`record_command_failed`] instead, so the two numbers together give
/// both the true delivery count and the retry volume.
pub(crate) fn record_command_processed(sri: &Sri, cmd: &str) {
    METRICS
        .commands
        .add(1, &attributes(sri, "cmd", "command", cmd));
}

/// Records that `sri`'s handler for `cmd` failed, so the command will be
/// redelivered. See [`record_command_processed`] for why this is separate.
pub(crate) fn record_command_failed(sri: &Sri, cmd: &str) {
    METRICS
        .commands_failed
        .add(1, &attributes(sri, "cmd", "command", cmd));
}

/// Records the wall time one handler's commit round trip took.
///
/// A handler runs in microseconds, so this is what a command's service time
/// actually is, and therefore what a cell's throughput ceiling is made of.
/// Kept as a sum and a count because the harness aggregates counters; a mean
/// settles the only question being asked of it, which is whether a round trip
/// costs about a millisecond or about seven.
pub(crate) fn record_commit(sri: &Sri, elapsed: std::time::Duration) {
    let attrs = [
        KeyValue::new("sri", sri.to_string()),
        KeyValue::new("kind", "command"),
        KeyValue::new("pid", std::process::id().to_string()),
    ];

    METRICS.commit_nanos.add(
        u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
        &attrs,
    );
    METRICS.commits.add(1, &attrs);
}

/// Records that `sri` finished processing an event named `event`.
pub(crate) fn record_event_processed(sri: &Sri, event: &str) {
    METRICS
        .events
        .add(1, &attributes(sri, "event", "event", event));
}

fn attributes(sri: &Sri, name_key: &'static str, kind: &'static str, name: &str) -> [KeyValue; 4] {
    [
        KeyValue::new("sri", sri.to_string()),
        KeyValue::new("kind", kind),
        KeyValue::new(name_key, name.to_owned()),
        KeyValue::new("pid", std::process::id().to_string()),
    ]
}
