use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use cell_protocol::{PlacementKind, Sri, scope_of_cell, scope_of_event};
pub use db_client::v1::models::Scope;
use db_client::{
    Session,
    v1::{Client, models::Subject},
};
use swarm_telemetry::db::opentelemetry_proto::tonic::metrics::v1::Metric;
use swarm_telemetry::db::opentelemetry_proto::tonic::{logs::v1::LogRecord, trace::v1::Span};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::SriAttribute;
use crate::metrics::CellInteractionMetricsSnapshot;

/// A ground-truth command-backlog snapshot for one cell — see [`DbHandle::cell_db_state`].
pub struct CellDbState {
    pub sri: Sri,
    /// commands still sitting in this cell's mailbox, unprocessed.
    pub commands_remaining: usize,
}

/// A ground-truth event-count snapshot for one event name/topic — see
/// [`DbHandle::event_topic_state`].
pub struct EventTopicState {
    pub event: String,
    /// events ever published under this name — never deleted, so this is a permanent total.
    pub produced: usize,
}

/// Returns true if every hop's candidate set in `expected_hops` matches at least one SRI in
/// `sris` — i.e. every hop the caller cares about is actually represented, not just "something
/// arrived". Each hop is a *set* of acceptable SRIs (e.g. every zone instance load-balancing
/// could have routed to) rather than a single SRI, since a UUID-derived SRI carries no
/// name/prefix structure to match on.
fn has_all_hops(sris: impl Iterator<Item = Sri>, expected_hops: &[Vec<Sri>]) -> bool {
    let seen: Vec<Sri> = sris.collect();
    expected_hops
        .iter()
        .all(|candidates| candidates.iter().any(|sri| seen.contains(sri)))
}

/// Returns true if `spans` (a single call's spans) cover every hop in `expected_hops`. Public
/// wrapper around `has_all_hops` for callers that already have spans in hand (e.g. from
/// [`DbHandle::query_spans_for_traces`]) and don't need to poll the DB per call.
#[must_use]
pub fn spans_cover_hops(spans: &[Span], expected_hops: &[Vec<Sri>]) -> bool {
    has_all_hops(spans.iter().filter_map(SriAttribute::sri), expected_hops)
}

/// Decodes a big-endian 16-byte distributed-tracing trace id, as stored on spans/log records.
pub(crate) fn trace_id_of(bytes: &[u8]) -> Option<Uuid> {
    bytes
        .try_into()
        .ok()
        .map(u128::from_be_bytes)
        .map(Uuid::from_u128)
}

/// Thin wrapper around the swarm DB client for use in tests, manages transaction handling.
pub struct DbHandle {
    client: Client,
    session: Session,
}

impl DbHandle {
    /// Create a handle on top of an existing zenoh session.
    pub fn new(session: &Session) -> Self {
        Self {
            client: Client::new(session),
            #[allow(clippy::clone_on_copy)]
            session: session.clone(),
        }
    }

    /// Insert raw bytes at the given scope + key.
    pub async fn put(&self, scope: Scope, key: &str, value: Vec<u8>) {
        let key = key.to_owned();
        self.client
            .write_tx_in(scope.clone(), async move |client, tx| {
                client
                    .send(db_client::v1::models::key_put::Request {
                        id: tx,
                        op: db_client::v1::models::key_put::Op { scope, key, value },
                    })
                    .await
            })
            .await
            .expect("db key_put failed")
            .expect("db key_put returned error");
    }

    /// Insert a UTF-8 string value
    pub async fn put_str(&self, scope: Scope, key: &str, value: &str) {
        self.put(scope, key, value.as_bytes().to_vec()).await;
    }

    /// Read raw bytes from the given scope + key.
    pub async fn get(&self, scope: Scope, key: &str) -> Option<Vec<u8>> {
        let key = key.to_owned();
        self.client
            .read_tx_in(scope.clone(), async move |client, tx| {
                client
                    .send(db_client::v1::models::key_get::Request {
                        id: tx,
                        op: db_client::v1::models::key_get::Op { scope, key },
                    })
                    .await
            })
            .await
            .expect("db key_get failed")
            .expect("db key_get returned error")
            .value
    }

    /// List every `(entry-id, value)` row in a table.
    pub async fn tb_list(&self, scope: Scope, table: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
        let table = table.to_owned();
        self.client
            .read_tx_in(scope.clone(), async move |client, tx| {
                Ok(sorg_common::tb_list(client.clone(), tx, scope, table, None, None, None).await)
            })
            .await
            .expect("db tb_list failed")
            .expect("db tb_list returned error")
    }

    /// Query every persisted entity in `table` under the shared telemetry scope, decoding each
    /// row as a `ScopedEntry<T>` and dropping rows that fail to decode.
    async fn query_table<T: serde::de::DeserializeOwned + serde::Serialize + std::fmt::Debug>(
        &self,
        table: &'static str,
    ) -> Vec<T> {
        let entities = self
            .client
            .read_tx_in(swarm_telemetry::db::scope(), async move |client, tx| {
                Ok(sorg_common::tb_list(
                    client.clone(),
                    tx,
                    swarm_telemetry::db::scope(),
                    table.to_owned(),
                    None,
                    None,
                    None,
                )
                .await)
            })
            .await
            .expect("db tb_list failed")
            .expect("db tb_list returned error");

        entities
            .into_iter()
            .filter_map(|(_id, data)| {
                serde_json::from_slice::<swarm_telemetry::db::ScopedEntry<T>>(&data).ok()
            })
            .map(|entry| entry.data)
            .collect()
    }

    /// Query every persisted log record, unfiltered.
    ///
    /// Requires the `swarm` binary to have been built with the `telemetry-export-db` feature
    /// (`swarm-telemetry/export-db`) otherwise this always returns an empty list, since nothing
    /// ever writes to the log table.
    async fn query_logs(&self) -> Vec<LogRecord> {
        self.query_table(swarm_telemetry::db::TABLE_LOGS).await
    }

    /// Query the telemetry log table for records whose distributed-tracing `trace_id` matches
    /// `trace_id`.
    pub async fn query_logs_for_trace(&self, trace_id: Uuid) -> Vec<LogRecord> {
        self.query_logs()
            .await
            .into_iter()
            .filter(|record| trace_id_of(&record.trace_id) == Some(trace_id))
            .collect()
    }

    /// Query every persisted span, unfiltered.
    async fn query_all_spans(&self) -> Vec<Span> {
        self.query_table(swarm_telemetry::db::TABLE_TRACES).await
    }

    pub async fn query_spans(&self, trace_id: Uuid) -> Vec<Span> {
        self.query_all_spans()
            .await
            .into_iter()
            .filter(|record| trace_id_of(&record.trace_id) == Some(trace_id))
            .collect()
    }

    /// Query every span named `name`, regardless of which trace it belongs to — for spans that
    /// don't share a specific call's trace (e.g. an internal operation covering many calls at
    /// once, like a batch DB poll), scanning by name is the only way to retrieve them;
    /// [`Self::query_spans_for_traces`] only finds spans whose trace id is already known.
    pub async fn query_spans_by_name(&self, name: &str) -> Vec<Span> {
        self.query_all_spans()
            .await
            .into_iter()
            .filter(|span| span.name == name)
            .collect()
    }

    /// Query spans for many trace ids in a single table scan, grouping by trace id. Calling
    /// [`Self::query_spans`] once per id re-scans the whole trace table each time — fine for a
    /// handful of calls, but O(calls × table size) for a benchmark producing hundreds of them.
    pub async fn query_spans_for_traces(&self, trace_ids: &[Uuid]) -> HashMap<Uuid, Vec<Span>> {
        let wanted: HashSet<Uuid> = trace_ids.iter().copied().collect();
        let mut grouped: HashMap<Uuid, Vec<Span>> = HashMap::new();

        for span in self.query_all_spans().await {
            if let Some(trace_id) = trace_id_of(&span.trace_id).filter(|id| wanted.contains(id)) {
                grouped.entry(trace_id).or_default().push(span);
            }
        }

        grouped
    }

    /// Groups every span with `start_time_unix_nano >= since` by trace id — for discovering
    /// calls a producer running *inside* the swarm generated itself, which never had an
    /// externally dispatched call to correlate by a known trace id (unlike
    /// [`Self::query_spans_for_traces`]).
    pub async fn query_spans_grouped_since(&self, since: u64) -> HashMap<Uuid, Vec<Span>> {
        let mut grouped: HashMap<Uuid, Vec<Span>> = HashMap::new();

        for span in self.query_all_spans().await {
            if span.start_time_unix_nano < since {
                continue;
            }
            if let Some(trace_id) = trace_id_of(&span.trace_id) {
                grouped.entry(trace_id).or_default().push(span);
            }
        }

        grouped
    }

    /// Query the latest exported metric snapshot from the telemetry DB.
    ///
    /// Requires the `swarm` binary to have been built with the `telemetry-export-db` feature
    /// (`swarm-telemetry/export-db`) otherwise this always returns an empty list, since nothing
    /// ever writes to the metric tables.
    pub async fn query_latest_metrics(&self) -> Vec<Metric> {
        self.query_table(swarm_telemetry::db::TABLE_METRICS_LATEST)
            .await
    }

    /// Read the latest exported cell command/event counters from the telemetry DB.
    pub async fn cell_interaction_metrics(&self) -> CellInteractionMetricsSnapshot {
        CellInteractionMetricsSnapshot::from_metrics(&self.query_latest_metrics().await)
    }

    /// Sums `tb_count` (a real, live DB row count — not an exported/derived metric) across every
    /// currently-deployed, non-placeholder cell's command mailbox table: how many commands are
    /// still waiting to be processed. A mailbox row is only ever removed by the poll that
    /// consumes it (see `cell-mailbox`'s `CommandStream::poll_batch`); no TTL or GC touches this
    /// table, so a row that has become invisible to a mailbox's cursor (the begin-timestamp/
    /// commit-order race documented on
    /// [`crate::scenario::SwarmTestCtx::wait_for_completeness`]) was never polled, hence never
    /// deleted, and stays counted here — unlike `commands_received`, which reflects only what a
    /// cursor has managed to see.
    ///
    /// Read per scope from a holder of that scope, which `Self::tb_count` explains is
    /// load-bearing: it was not, and the resulting count could not reach zero.
    ///
    /// Still not a total: the drain copies rows to a new holder without deleting the originals,
    /// so a row mid-drain can be counted once, twice, or not at all depending on which holder
    /// each read resolves to. Treat a nonzero result as "work outstanding somewhere", never as
    /// an exact backlog.
    ///
    /// Deployed cells are read from the swarm's own cell registry (`sorg_common::list_cells`),
    /// not from whatever the test harness thinks it dispatched — this also covers cells that
    /// generate their own load internally (e.g. a timer-driven producer with no externally
    /// dispatched call to count) rather than only ones the harness drove itself.
    pub async fn command_backlog(&self) -> usize {
        let cells = self.deployed_cells().await;

        // Concurrent, not sequential: one round trip's worth of wall time for every cell's count,
        // not N of them back to back. Sequentially, this loop's own duration could rival
        // `poll_interval` once there are more than a handful of cells, turning what's meant to be
        // an occasional check into a near-continuous stream of read transactions competing with
        // the very command traffic it's trying to observe — extra load on the store for the
        // entire observation window, not just a brief poll.
        let counts = futures::future::join_all(
            cells
                .into_iter()
                .map(|sri| self.tb_count(scope_of_cell(sri), cell_protocol::MESSAGES_TABLE)),
        )
        .await;

        counts.into_iter().sum()
    }

    /// A ground-truth command-backlog snapshot per currently-deployed cell — how many commands
    /// are still sitting unprocessed in its mailbox; see [`Self::command_backlog`] for why this
    /// is reliable even under the cursor-visibility race.
    pub async fn cell_db_state(&self) -> Vec<CellDbState> {
        let cells = self.deployed_cells().await;

        futures::future::join_all(cells.into_iter().map(|sri| async move {
            let commands_remaining = self
                .tb_count(scope_of_cell(sri), cell_protocol::MESSAGES_TABLE)
                .await;
            CellDbState {
                sri,
                commands_remaining,
            }
        }))
        .await
    }

    /// A ground-truth event-count snapshot per event name/topic currently in use — how many
    /// events have ever been published under that name, read directly off the DB's own event
    /// tables rather than an exported/derived metric. Events are never deleted (`cell-mailbox`'s
    /// event stream is an append-only log, not a queue), so this is a true, permanent total, not
    /// a snapshot that could regress.
    ///
    /// Unlike commands, events aren't scoped per cell in the DB: every publisher of a given event
    /// name shares one table (`cell_protocol::scope_of_event`), so there's no per-cell breakdown
    /// to give — topic names are discovered from the exported `cell_events_sent` metric's `event`
    /// attribute ([`crate::metrics::event_names`]) rather than hardcoded, so this works for any
    /// scenario's event names without the caller needing to know them upfront.
    pub async fn event_topic_state(&self) -> Vec<EventTopicState> {
        let names = crate::metrics::event_names(&self.query_latest_metrics().await);
        self.event_topic_state_for(names).await
    }

    /// [`Self::event_topic_state`] for callers that already know the event
    /// names (e.g. discovered from file-exported metrics rather than the db's
    /// `metrics_latest` table) — the counts themselves are always live db
    /// reads, whatever backend telemetry exports to.
    pub async fn event_topic_state_for(
        &self,
        names: std::collections::BTreeSet<String>,
    ) -> Vec<EventTopicState> {
        futures::future::join_all(names.into_iter().map(|event| async move {
            let produced = self
                .tb_count(scope_of_event(&event), cell_protocol::EVENTS_TABLE)
                .await;
            EventTopicState { event, produced }
        }))
        .await
    }

    /// Every currently-deployed, non-placeholder cell's `Sri`, read from the swarm's own placement
    /// registry (`sorg_common::list_placements`) rather than from whatever the test harness thinks it
    /// dispatched — this also covers cells that generate load internally (e.g. a timer-driven
    /// producer with no externally dispatched call to count) rather than only ones the harness
    /// drove itself.
    async fn deployed_cells(&self) -> Vec<Sri> {
        sorg_common::list_placements(&self.session)
            .await
            .expect("failed to list placement registry")
            .into_iter()
            .filter(|cell| !matches!(cell.kind, PlacementKind::Placeholder))
            .map(|cell| cell.sri)
            .collect()
    }

    /// A live row count for `scope`'s `table`, read from a holder of that scope.
    ///
    /// Routed deliberately. An unrouted read defaults to `Constraint::Ignore`, which resolves
    /// through `any_node` — and `any_node` sorts node ids and pops the max, so *every* scope's
    /// count came back from one arbitrary host regardless of who held it. That host is also
    /// where stray cross-scope rows accumulate (a transaction anchored there appends into
    /// another cell's scope, and the drain copies those rows onward without deleting them), so
    /// the sum converged to a nonzero constant and every rack pass reported
    /// [`Completeness::Stalled`](crate::scenario::Completeness::Stalled) at every load — a
    /// measurement artifact, not a stuck row.
    async fn tb_count(&self, scope: Scope, table: &str) -> usize {
        let table = table.to_owned();
        self.client
            .read_tx_in(scope.clone(), async move |client, tx_id| {
                Ok(sorg_common::tb_count(client.clone(), tx_id, scope, table).await)
            })
            .await
            .expect("db tb_count failed")
            .expect("db tb_count returned error")
    }

    /// Poll [`Self::query_spans`] until a span's `sri` attribute has matched every hop's
    /// candidate set in `expected_hops`, or `max_attempts` polls (1s apart) have elapsed. Each
    /// hop is a set of acceptable SRIs (e.g. every zone replica's SRI), which is needed once
    /// load is distributed across replicas and the caller can't predict which shard id will
    /// handle a given hop. Returns whether every expected hop was seen, alongside the
    /// last-fetched spans — so callers can inspect what was (or wasn't) found even on a
    /// timeout. Checking for the specific hops you expect (rather than just "is the list
    /// non-empty") means a missing or duplicated hop can't silently pass as success.
    pub async fn await_span_hops(
        &self,
        trace_id: Uuid,
        expected_hops: &[Vec<Sri>],
        max_attempts: u32,
    ) -> (bool, Vec<Span>) {
        let mut spans = Vec::new();
        for attempt in 1..=max_attempts {
            spans = self.query_spans(trace_id).await;
            if has_all_hops(spans.iter().filter_map(SriAttribute::sri), expected_hops) {
                return (true, spans);
            }
            if attempt < max_attempts {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    self.await_span_insertion(),
                )
                .await;
            }
        }
        (false, spans)
    }

    pub async fn await_span_insertion(&self) {
        let notify = Arc::new(Notify::new());
        let notifier = notify.clone();
        let _subscription = self.client.subscribe(
            Subject::Database(
                swarm_telemetry::db::scope().namespace,
                swarm_telemetry::db::DATABASE.into(),
            ),
            swarm_telemetry::db::TABLE_TRACES,
            move |_| notifier.notify_one(),
        );

        notify.notified().await;
    }
}
