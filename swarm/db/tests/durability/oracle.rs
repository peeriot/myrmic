//! Ground truth for what the cluster was told to do, and the validation
//! sweep that compares it against what the cluster actually holds.
//!
//! The oracle lives in the orchestrator, which never dies, so its ledger of
//! acked transactions is trustworthy by construction.

use crate::cluster::{ClusterState, scope_key};
use crate::proto::{Op, TxId};
use std::collections::{BTreeMap, HashSet};

/// `(scope, key)` — one logical cell of the keyspace.
type Slot = (String, String);
/// scope → key → value, as dumped from one node.
type Dump = BTreeMap<String, BTreeMap<String, String>>;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Submitted; no result seen yet.
    InFlight,
    /// Worker acked the commit. Must survive anything short of disk loss.
    Committed { ts: u64 },
    /// Worker reported the commit failed (e.g. write conflict). Its writes
    /// must never be visible anywhere.
    Failed,
    /// The node died before reporting. May have committed or not — but only
    /// atomically.
    Indeterminate,
}

#[derive(Debug, Clone)]
pub struct TxRecord {
    pub id: TxId,
    pub origin: String,
    pub ops: Vec<Op>,
    pub retention: bool,
    pub status: Status,
}

#[derive(Default)]
pub struct Oracle {
    txs: BTreeMap<TxId, TxRecord>,
    next_id: TxId,
}

impl Oracle {
    pub fn begin(&mut self, origin: &str, ops: Vec<Op>, retention: bool) -> TxId {
        self.next_id += 1;
        let id = self.next_id;
        self.txs.insert(
            id,
            TxRecord {
                id,
                origin: origin.to_string(),
                ops,
                retention,
                status: Status::InFlight,
            },
        );
        id
    }

    pub fn on_result(&mut self, id: TxId, ts: u64, ok: bool, error: Option<&str>) {
        let record = self.txs.get_mut(&id).expect("result for unknown tx");
        debug_assert!(record.status == Status::InFlight, "tx {id} resolved twice");
        record.status = if ok {
            Status::Committed { ts }
        } else {
            tracing::debug!("tx {id} failed: {error:?}");
            Status::Failed
        };
    }

    /// Every in-flight tx on a node that just died becomes indeterminate.
    pub fn mark_indeterminate(&mut self, origin: &str) {
        for record in self.txs.values_mut() {
            if record.origin == origin && record.status == Status::InFlight {
                record.status = Status::Indeterminate;
            }
        }
    }

    /// Transactions that have a definite outcome (committed or failed).
    pub fn resolved_count(&self) -> usize {
        self.txs
            .values()
            .filter(|r| matches!(r.status, Status::Committed { .. } | Status::Failed))
            .count()
    }

    pub fn in_flight_count(&self) -> usize {
        self.txs
            .values()
            .filter(|r| r.status == Status::InFlight)
            .count()
    }

    pub fn status_summary(&self) -> String {
        let mut committed = 0;
        let mut failed = 0;
        let mut indeterminate = 0;
        let mut in_flight = 0;
        for record in self.txs.values() {
            match record.status {
                Status::Committed { .. } => committed += 1,
                Status::Failed => failed += 1,
                Status::Indeterminate => indeterminate += 1,
                Status::InFlight => in_flight += 1,
            }
        }
        format!(
            "{} txs: {committed} committed, {failed} failed, {indeterminate} indeterminate, {in_flight} in flight",
            self.txs.len()
        )
    }

    /// Validate a converged cluster state against the ledger. Returns the
    /// list of invariant violations (empty = pass).
    pub fn validate(&self, state: &ClusterState, opts: &ValidateOpts) -> Vec<String> {
        let mut violations = vec![];

        // Convergence is settle's job, but assert it here too so validate()
        // alone is a complete check.
        if let Some(divergence) = state.divergence(opts.ignore_key_prefix) {
            violations.push(format!("nodes diverge: {divergence}"));
        }

        let Some(reference) = state.dumps.values().next() else {
            violations.push("no live nodes to validate".to_string());
            return violations;
        };

        let proven_ts = self.check_provenance(reference, opts, &mut violations);
        let expected = self.expectations(&proven_ts, opts);
        check_winners(&expected, reference, &mut violations);
        self.check_atomicity(&proven_ts, &expected, reference, opts, &mut violations);
        if opts.check_frontier {
            self.check_frontier(state, opts, &mut violations);
        }

        violations.sort();
        violations.dedup();
        violations
    }

    /// Pass 1: provenance. Every observed value must parse and trace to a
    /// transaction that was allowed to commit, at the ts it reported.
    /// Indeterminate txs with visible effects are thereby proven committed;
    /// their learned commit ts is returned.
    fn check_provenance(
        &self,
        reference: &Dump,
        opts: &ValidateOpts,
        violations: &mut Vec<String>,
    ) -> BTreeMap<TxId, u64> {
        let mut proven_ts = BTreeMap::new();

        for (scope, entries) in reference {
            for (key, value) in entries {
                if opts.ignores(key) {
                    continue;
                }

                let Some((tx_id, value_ts)) = parse_value(value) else {
                    violations.push(format!("{scope}:{key} holds unparseable value {value:?}"));
                    continue;
                };
                let Some(record) = self.txs.get(&tx_id) else {
                    violations.push(format!("{scope}:{key} holds value from unknown tx {tx_id}"));
                    continue;
                };

                match record.status {
                    Status::Committed { ts } if ts != value_ts => violations.push(format!(
                        "{scope}:{key}: tx {tx_id} committed at ts {ts} but value claims {value_ts}"
                    )),
                    Status::Committed { .. } => {}
                    Status::Failed => violations.push(format!(
                        "{scope}:{key}: zombie write from failed tx {tx_id}"
                    )),
                    Status::Indeterminate => {
                        proven_ts.insert(tx_id, value_ts);
                    }
                    Status::InFlight => violations.push(format!(
                        "{scope}:{key}: tx {tx_id} still unresolved during validation"
                    )),
                }
            }
        }

        proven_ts
    }

    /// Derive what every key should show from the committed transaction set.
    fn expectations<'a>(
        &'a self,
        proven_ts: &BTreeMap<TxId, u64>,
        opts: &ValidateOpts,
    ) -> Expectations<'a> {
        let mut expected = Expectations::default();

        for record in self.txs.values() {
            if opts.skip_retention && record.retention {
                continue;
            }
            let ts = committed_ts(record, proven_ts);
            for op in &record.ops {
                if opts.ignores(op.key()) {
                    continue;
                }
                let slot = (scope_key(op.scope()), op.key().to_string());

                match ts {
                    Some(ts) => {
                        if matches!(op, Op::Del { .. }) {
                            expected
                                .deleted_at
                                .entry(slot.clone())
                                .or_default()
                                .push(ts);
                        }
                        let better = expected
                            .winners
                            .get(&slot)
                            .is_none_or(|current| ts > current.ts);
                        if better {
                            expected.winners.insert(
                                slot,
                                Winner {
                                    ts,
                                    tx_id: record.id,
                                    put_value: match op {
                                        Op::Put { payload, .. } => {
                                            Some((payload.as_str(), record.origin.as_str()))
                                        }
                                        Op::Del { .. } => None,
                                    },
                                },
                            );
                        }
                    }
                    None if record.status == Status::Indeterminate => {
                        if matches!(op, Op::Del { .. }) {
                            expected.maybe_deleted.insert(slot);
                        }
                    }
                    None => {}
                }
            }
        }

        expected
    }

    /// Pass 3: atomicity. A committed (or proven-committed) tx must be
    /// all-or-nothing: each of its writes is either visible or shadowed by
    /// a strictly newer version — never simply missing.
    fn check_atomicity(
        &self,
        proven_ts: &BTreeMap<TxId, u64>,
        expected: &Expectations<'_>,
        reference: &Dump,
        opts: &ValidateOpts,
        violations: &mut Vec<String>,
    ) {
        for record in self.txs.values() {
            if opts.skip_retention && record.retention {
                continue;
            }
            // Proven-committed indeterminates are held to atomicity too.
            let Some(ts) = committed_ts(record, proven_ts) else {
                continue;
            };

            for op in &record.ops {
                let scope = scope_key(op.scope());
                let key = op.key();
                if opts.ignores(key) {
                    continue;
                }

                let observed = reference.get(&scope).and_then(|entries| entries.get(key));
                let observed_ts = observed.and_then(|v| parse_value(v)).map(|(_, ts)| ts);

                let slot = (scope.clone(), key.to_string());
                let intact = match op {
                    // Our own version or anything newer satisfies the put.
                    // Absence is fine too when explained by a newer committed
                    // delete, or by an unproven delete from a dead node.
                    Op::Put { .. } => {
                        observed_ts.is_some_and(|seen| seen >= ts)
                            || (observed_ts.is_none()
                                && (expected.maybe_deleted.contains(&slot)
                                    || expected
                                        .deleted_at
                                        .get(&slot)
                                        .is_some_and(|dels| dels.iter().any(|&del| del > ts))))
                    }
                    // Absence or anything strictly newer satisfies the delete.
                    Op::Del { .. } => observed_ts.is_none_or(|seen| seen > ts),
                };

                if !intact {
                    violations.push(format!(
                        "{scope}:{key}: ATOMICITY — tx {} (ts {ts}) lost this {} while the tx counts as committed (observed ts {observed_ts:?})",
                        record.id,
                        match op {
                            Op::Put { .. } => "put",
                            Op::Del { .. } => "delete",
                        },
                    ));
                }
            }
        }
    }

    /// Pass 4: frontier completeness. Every committed tx produced a sync
    /// point at its ts in each scope it touched; after convergence every
    /// node must know that version.
    fn check_frontier(
        &self,
        state: &ClusterState,
        opts: &ValidateOpts,
        violations: &mut Vec<String>,
    ) {
        for (node, heads) in &state.heads {
            let known: HashSet<(String, u64)> =
                heads.iter().map(|h| (scope_key(&h.scope), h.ts)).collect();

            for record in self.txs.values() {
                let Status::Committed { ts } = record.status else {
                    continue;
                };
                if opts.skip_retention && record.retention {
                    continue;
                }
                for op in &record.ops {
                    let scope = scope_key(op.scope());
                    if !known.contains(&(scope.clone(), ts)) {
                        violations.push(format!(
                            "frontier on {node} is missing sync point ts {ts} for {scope} (tx {})",
                            record.id
                        ));
                    }
                }
            }
        }
    }
}

/// The winning (highest-ts committed) write for a key.
#[derive(Debug)]
struct Winner<'a> {
    ts: u64,
    tx_id: TxId,
    /// `Some((payload, origin))` for a put, `None` for a delete.
    put_value: Option<(&'a str, &'a str)>,
}

#[derive(Default)]
struct Expectations<'a> {
    /// slot → winner among committed txs.
    winners: BTreeMap<Slot, Winner<'a>>,
    /// slot → timestamps of committed deletes (absence witnesses).
    deleted_at: BTreeMap<Slot, Vec<u64>>,
    /// Slots an unproven indeterminate tx deleted.
    ///
    /// An indeterminate tx with any visible put is proven committed by
    /// pass 1, so the only unresolvable case is a tx whose puts were all
    /// shadowed (or that only deleted): its delete may have legitimately
    /// removed a key we'd otherwise expect to see.
    maybe_deleted: HashSet<Slot>,
}

fn committed_ts(record: &TxRecord, proven_ts: &BTreeMap<TxId, u64>) -> Option<u64> {
    match record.status {
        Status::Committed { ts } => Some(ts),
        Status::Indeterminate => proven_ts.get(&record.id).copied(),
        _ => None,
    }
}

/// Pass 2: compare each key's winner against what the cluster shows.
fn check_winners(expected: &Expectations<'_>, reference: &Dump, violations: &mut Vec<String>) {
    for (slot @ (scope, key), winner) in &expected.winners {
        let observed = reference.get(scope).and_then(|entries| entries.get(key));

        match (winner.put_value, observed) {
            (Some((payload, _)), Some(observed)) => {
                let expectation = format!("{}:{}:{payload}", winner.tx_id, winner.ts);
                if *observed != expectation {
                    violations.push(format!(
                        "{scope}:{key}: expected {expectation:?} (winning tx {}), found {observed:?}",
                        winner.tx_id
                    ));
                }
            }
            (Some((payload, origin)), None) => {
                // A dead node's unproven delete may have removed it.
                if !expected.maybe_deleted.contains(slot) {
                    violations.push(format!(
                        "{scope}:{key}: LOST WRITE — tx {} (acked on {origin}, ts {}) put {payload:?} but the key is gone",
                        winner.tx_id, winner.ts
                    ));
                }
            }
            (None, None) => {} // winning delete, key absent
            (None, Some(observed)) => violations.push(format!(
                "{scope}:{key}: tx {} deleted this key at ts {} but it resurrected as {observed:?}",
                winner.tx_id, winner.ts
            )),
        }
    }
}

pub struct ValidateOpts {
    pub check_frontier: bool,
    pub skip_retention: bool,
    pub ignore_key_prefix: Option<&'static str>,
}

impl ValidateOpts {
    fn ignores(&self, key: &str) -> bool {
        self.ignore_key_prefix
            .is_some_and(|prefix| key.starts_with(prefix))
    }
}

impl Default for ValidateOpts {
    fn default() -> Self {
        Self {
            check_frontier: true,
            skip_retention: false,
            ignore_key_prefix: None,
        }
    }
}

/// Values are written as `txid:ts:payload`.
fn parse_value(value: &str) -> Option<(TxId, u64)> {
    let mut parts = value.splitn(3, ':');
    let tx_id = parts.next()?.parse().ok()?;
    let ts = parts.next()?.parse().ok()?;
    parts.next()?;
    Some((tx_id, ts))
}
