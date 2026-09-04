//! The host-agnostic supervision core, shared by the Linux exec and the
//! embedded cell host: the child-side fencing decision table (spec §3) and
//! the observer-local lease staleness tracker. Pure and `no_std`: the host
//! gathers evidence (placement reads, lease scans) on its own cadence and
//! clock; this decides. One evidence rule throughout: only an affirmative,
//! successful read counts — a failed read is "unknown" and changes nothing.
//!
//! Time is a `u64` millisecond tick from any monotonic source (`Instant` on
//! Linux, the SoC tick on embedded); only differences are ever taken.

use crate::sys::vec::Vec;
use crate::{Gen, RuntimeId, SpawnLineage, Sri};

/// What a host's verification pass knows about one hosted cell: its
/// identity, its generation, and the spawn edge it was born on.
#[derive(Debug, Clone)]
pub struct WatchedCell {
    /// The hosted cell.
    pub sri: Sri,
    /// The generation this body was deployed under.
    pub gen_id: Gen,
    /// The spawn edge (parent, anchor, detachment, grace).
    pub lineage: SpawnLineage,
}

/// Outcome of reading a placement row this pass.
#[derive(Debug, Clone, Copy)]
pub enum RowRead<T> {
    /// The row exists and was decoded.
    Ok(T),
    /// A successful read found no row.
    Absent,
    /// Read error/timeout: no evidence, take no action.
    Failed,
}

/// Facts read from a placement row: the hosting exec and the row's
/// generation.
pub type RowFacts = (RuntimeId, Gen);

/// Facts read from a node's lease row: the renewal `seq` and the ttl the
/// writer declared for itself.
pub type LeaseFacts = (u64, u64);

/// One cell's evidence for one pass, as gathered by the host.
#[derive(Debug, Clone, Copy)]
pub struct Evidence {
    /// The cell's own placement row.
    pub self_row: RowRead<RowFacts>,
    /// The parent's placement row. Ignored for roots and detached cells.
    pub parent_row: RowRead<RowFacts>,
    /// Whether the parent's node lease is stale past this edge's grace.
    /// `None` = the parent's node has no lease row (not fenceable from this
    /// vantage — a purged row is hygiene's death evidence, not the child's).
    pub parent_lease_expired: Option<bool>,
}

/// Why a cell must die (the fencing kill causes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillWhy {
    /// The cell's own row is gone or names another body: this body lost.
    SelfSuperseded,
    /// The parent's row has been confirmed absent.
    ParentAbsent,
    /// The parent's row carries a different generation than this cell was
    /// born under: the parent was replaced.
    ParentSuperseded,
    /// The parent's node lease is stale past the edge's grace.
    ParentNodeDead,
}

/// The decision for one cell this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing refuted this cell; leave it running.
    Keep,
    /// Kill the cell locally and release its rows.
    Kill(KillWhy),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhichRow {
    SelfRow,
    ParentRow,
}

const CONFIRM_ABSENT_AFTER: u8 = 2;

/// Tracks consecutive confirmed-absent reads per (cell, row): "absent" only
/// counts as death after two successful reads in a row found nothing, riding
/// out replication reordering.
#[derive(Debug, Default)]
pub struct FencingState {
    absent_counts: Vec<(Sri, WhichRow, u8)>,
}

impl FencingState {
    /// An empty state: no absence streaks yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies spec §3 to one cell's evidence. Order: self-check first, then
    /// parent checks (skipped for roots and detached cells).
    pub fn evaluate(&mut self, my_exec: RuntimeId, cell: &WatchedCell, ev: &Evidence) -> Verdict {
        if let Some(why) = self.check_self(my_exec, cell, ev) {
            self.forget(&cell.sri);
            return Verdict::Kill(why);
        }

        if cell.lineage.parent.is_some()
            && !cell.lineage.detached
            && let Some(why) = self.check_parent(cell, ev)
        {
            self.forget(&cell.sri);
            return Verdict::Kill(why);
        }

        Verdict::Keep
    }

    fn check_self(
        &mut self,
        my_exec: RuntimeId,
        cell: &WatchedCell,
        ev: &Evidence,
    ) -> Option<KillWhy> {
        match ev.self_row {
            RowRead::Ok((exec, row_gen)) => {
                self.reset(&cell.sri, WhichRow::SelfRow);
                if exec != my_exec || row_gen != cell.gen_id {
                    return Some(KillWhy::SelfSuperseded);
                }
                None
            }
            RowRead::Absent => self
                .confirmed_absent(&cell.sri, WhichRow::SelfRow)
                .then_some(KillWhy::SelfSuperseded),
            RowRead::Failed => None,
        }
    }

    fn check_parent(&mut self, cell: &WatchedCell, ev: &Evidence) -> Option<KillWhy> {
        match ev.parent_row {
            RowRead::Ok((_, row_gen)) => {
                self.reset(&cell.sri, WhichRow::ParentRow);
                // Fires only when the child carries an anchor: rows from
                // external deploys have no parent generation to compare.
                if cell
                    .lineage
                    .parent_gen_id
                    .is_some_and(|anchor| anchor != row_gen)
                {
                    return Some(KillWhy::ParentSuperseded);
                }
                if ev.parent_lease_expired == Some(true) {
                    return Some(KillWhy::ParentNodeDead);
                }
                None
            }
            RowRead::Absent => self
                .confirmed_absent(&cell.sri, WhichRow::ParentRow)
                .then_some(KillWhy::ParentAbsent),
            RowRead::Failed => None,
        }
    }

    fn confirmed_absent(&mut self, sri: &Sri, row: WhichRow) -> bool {
        match self
            .absent_counts
            .iter_mut()
            .find(|(s, r, _)| s == sri && *r == row)
        {
            Some((_, _, count)) => {
                *count += 1;
                *count >= CONFIRM_ABSENT_AFTER
            }
            None => {
                self.absent_counts.push((*sri, row, 1));
                1 >= CONFIRM_ABSENT_AFTER
            }
        }
    }

    fn reset(&mut self, sri: &Sri, row: WhichRow) {
        self.absent_counts
            .retain(|(s, r, _)| !(s == sri && *r == row));
    }

    /// Drops all streaks for a cell (killed or undeployed).
    pub fn forget(&mut self, sri: &Sri) {
        self.absent_counts.retain(|(s, _, _)| s != sri);
    }
}

/// Observer-local lease staleness. Expiry is measured on the observer's own
/// monotonic tick from the last seq *advance* it saw, against the ttl the
/// node declared in that lease; wall clocks and row timestamps are never
/// compared. First sight counts as an advance, so a cold-started observer
/// errs late, never early.
#[derive(Debug, Default)]
pub struct LeaseTracker {
    seen: Vec<Observed>,
}

#[derive(Debug)]
struct Observed {
    id: RuntimeId,
    seq: u64,
    /// Observer tick of the last seq advance.
    advanced_at_ms: u64,
    /// The ttl the node declared in that lease.
    ttl_ms: u64,
}

impl LeaseTracker {
    /// An empty tracker; every node's expiry follows its own declared ttl.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one lease-scan result for a node.
    pub fn observe(&mut self, id: RuntimeId, seq: u64, ttl_ms: u64, now_ms: u64) {
        match self.seen.iter_mut().find(|o| o.id == id) {
            // Keeping the old tick on a non-advancing seq IS the staleness
            // mechanism; the stale lease's ttl keeps judging it.
            Some(o) => {
                if seq > o.seq {
                    o.seq = seq;
                    o.advanced_at_ms = now_ms;
                    o.ttl_ms = ttl_ms;
                }
            }
            None => self.seen.push(Observed {
                id,
                seq,
                advanced_at_ms: now_ms,
                ttl_ms,
            }),
        }
    }

    /// An unknown node is never expired: absence of lease evidence means
    /// "not fenceable", not "dead".
    pub fn is_expired(&self, id: RuntimeId, now_ms: u64) -> bool {
        self.seen
            .iter()
            .find(|o| o.id == id)
            .is_some_and(|o| now_ms.saturating_sub(o.advanced_at_ms) > o.ttl_ms)
    }

    /// How long since this observer last saw the node's lease advance;
    /// `None` for nodes it has never observed. Lets callers apply a
    /// per-edge tolerance instead of the node's declared ttl.
    pub fn stale_for(&self, id: RuntimeId, now_ms: u64) -> Option<u64> {
        self.seen
            .iter()
            .find(|o| o.id == id)
            .map(|o| now_ms.saturating_sub(o.advanced_at_ms))
    }

    /// The ttl an observed node declared in its last advancing lease.
    pub fn ttl_ms_of(&self, id: RuntimeId) -> Option<u64> {
        self.seen.iter().find(|o| o.id == id).map(|o| o.ttl_ms)
    }

    /// Folds one point-read of a node's lease row into the tracker and
    /// judges staleness against `grace_ms`, defaulting to the node's
    /// declared ttl when no per-edge grace is given. `Absent` means the
    /// node's lease is gone (not fenceable → `None`); `Failed` is no
    /// evidence — a prior observation keeps aging.
    pub fn judge_read(
        &mut self,
        id: RuntimeId,
        read: RowRead<LeaseFacts>,
        grace_ms: Option<u64>,
        now_ms: u64,
    ) -> Option<bool> {
        match read {
            RowRead::Ok((seq, ttl_ms)) => self.observe(id, seq, ttl_ms, now_ms),
            RowRead::Absent => return None,
            RowRead::Failed => {}
        }
        let tolerance = grace_ms.or_else(|| self.ttl_ms_of(id)).unwrap_or(u64::MAX);
        Some(self.stale_for(id, now_ms).is_some_and(|s| s > tolerance))
    }

    /// Every observed node whose lease has been silent past its declared ttl.
    pub fn expired(&self, now_ms: u64) -> Vec<RuntimeId> {
        self.seen
            .iter()
            .filter(|o| now_ms.saturating_sub(o.advanced_at_ms) > o.ttl_ms)
            .map(|o| o.id)
            .collect()
    }

    /// Stops tracking a node (torn down by hygiene).
    pub fn forget(&mut self, id: RuntimeId) {
        self.seen.retain(|o| o.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sri_of_path;

    fn rt(n: u8) -> RuntimeId {
        zenoh_protocol::core::ZenohIdProto::try_from(&[n; 8][..])
            .unwrap()
            .into()
    }

    fn sri(name: &str) -> Sri {
        sri_of_path(name).unwrap().into()
    }

    fn id(n: u128) -> Gen {
        Gen::from_parts(0, n)
    }

    fn cell() -> WatchedCell {
        WatchedCell {
            sri: sri("child"),
            gen_id: id(1),
            lineage: SpawnLineage {
                parent: Some(sri("parent")),
                parent_gen_id: Some(id(2)),
                ..Default::default()
            },
        }
    }

    fn healthy(me: RuntimeId) -> Evidence {
        Evidence {
            self_row: RowRead::Ok((me, id(1))),
            parent_row: RowRead::Ok((rt(9), id(2))),
            parent_lease_expired: Some(false),
        }
    }

    #[test]
    fn healthy_cell_keeps() {
        let me = rt(1);
        let mut f = FencingState::new();
        assert_eq!(f.evaluate(me, &cell(), &healthy(me)), Verdict::Keep);
    }

    #[test]
    fn self_row_absent_twice_kills() {
        let me = rt(1);
        let mut f = FencingState::new();
        let ev = Evidence {
            self_row: RowRead::Absent,
            ..healthy(me)
        };
        assert_eq!(f.evaluate(me, &cell(), &ev), Verdict::Keep);
        assert_eq!(
            f.evaluate(me, &cell(), &ev),
            Verdict::Kill(KillWhy::SelfSuperseded)
        );
    }

    #[test]
    fn self_row_absent_then_present_resets_counter() {
        let me = rt(1);
        let mut f = FencingState::new();
        let absent = Evidence {
            self_row: RowRead::Absent,
            ..healthy(me)
        };
        assert_eq!(f.evaluate(me, &cell(), &absent), Verdict::Keep);
        assert_eq!(f.evaluate(me, &cell(), &healthy(me)), Verdict::Keep);
        assert_eq!(f.evaluate(me, &cell(), &absent), Verdict::Keep);
    }

    #[test]
    fn self_row_names_other_exec_kills_immediately() {
        let me = rt(1);
        let mut f = FencingState::new();
        let ev = Evidence {
            self_row: RowRead::Ok((rt(2), id(1))),
            ..healthy(me)
        };
        assert_eq!(
            f.evaluate(me, &cell(), &ev),
            Verdict::Kill(KillWhy::SelfSuperseded)
        );
    }

    #[test]
    fn self_row_instance_mismatch_kills_immediately() {
        let me = rt(1);
        let mut f = FencingState::new();
        let ev = Evidence {
            self_row: RowRead::Ok((me, id(99))),
            ..healthy(me)
        };
        assert_eq!(
            f.evaluate(me, &cell(), &ev),
            Verdict::Kill(KillWhy::SelfSuperseded)
        );
    }

    #[test]
    fn read_failure_is_no_evidence_and_does_not_reset() {
        let me = rt(1);
        let mut f = FencingState::new();
        let absent = Evidence {
            self_row: RowRead::Absent,
            ..healthy(me)
        };
        let failed = Evidence {
            self_row: RowRead::Failed,
            parent_row: RowRead::Failed,
            parent_lease_expired: None,
        };
        assert_eq!(f.evaluate(me, &cell(), &absent), Verdict::Keep);
        // Failed read: neither confirms nor resets the absent streak.
        assert_eq!(f.evaluate(me, &cell(), &failed), Verdict::Keep);
        assert_eq!(
            f.evaluate(me, &cell(), &absent),
            Verdict::Kill(KillWhy::SelfSuperseded)
        );
    }

    #[test]
    fn parent_row_absent_twice_kills() {
        let me = rt(1);
        let mut f = FencingState::new();
        let ev = Evidence {
            parent_row: RowRead::Absent,
            ..healthy(me)
        };
        assert_eq!(f.evaluate(me, &cell(), &ev), Verdict::Keep);
        assert_eq!(
            f.evaluate(me, &cell(), &ev),
            Verdict::Kill(KillWhy::ParentAbsent)
        );
    }

    #[test]
    fn parent_instance_mismatch_kills_immediately() {
        let me = rt(1);
        let mut f = FencingState::new();
        let ev = Evidence {
            parent_row: RowRead::Ok((rt(9), id(77))),
            ..healthy(me)
        };
        assert_eq!(
            f.evaluate(me, &cell(), &ev),
            Verdict::Kill(KillWhy::ParentSuperseded)
        );
    }

    #[test]
    fn parent_node_lease_expired_kills() {
        let me = rt(1);
        let mut f = FencingState::new();
        let ev = Evidence {
            parent_lease_expired: Some(true),
            ..healthy(me)
        };
        assert_eq!(
            f.evaluate(me, &cell(), &ev),
            Verdict::Kill(KillWhy::ParentNodeDead)
        );
    }

    #[test]
    fn parent_without_lease_row_is_alive() {
        let me = rt(1);
        let mut f = FencingState::new();
        let ev = Evidence {
            parent_lease_expired: None,
            ..healthy(me)
        };
        assert_eq!(f.evaluate(me, &cell(), &ev), Verdict::Keep);
    }

    #[test]
    fn detached_skips_parent_checks_but_not_self_check() {
        let me = rt(1);
        let mut f = FencingState::new();
        let mut c = cell();
        c.lineage.detached = true;
        let parent_dead = Evidence {
            parent_row: RowRead::Absent,
            parent_lease_expired: Some(true),
            ..healthy(me)
        };
        assert_eq!(f.evaluate(me, &c, &parent_dead), Verdict::Keep);
        assert_eq!(f.evaluate(me, &c, &parent_dead), Verdict::Keep);

        let superseded = Evidence {
            self_row: RowRead::Ok((me, id(99))),
            ..healthy(me)
        };
        assert_eq!(
            f.evaluate(me, &c, &superseded),
            Verdict::Kill(KillWhy::SelfSuperseded)
        );
    }

    #[test]
    fn root_skips_parent_checks() {
        let me = rt(1);
        let mut f = FencingState::new();
        let mut c = cell();
        c.lineage.parent = None;
        c.lineage.parent_gen_id = None;
        let ev = Evidence {
            parent_row: RowRead::Absent,
            parent_lease_expired: Some(true),
            ..healthy(me)
        };
        assert_eq!(f.evaluate(me, &c, &ev), Verdict::Keep);
        assert_eq!(f.evaluate(me, &c, &ev), Verdict::Keep);
    }

    #[test]
    fn unanchored_parent_gen_is_not_compared() {
        // External deploys declare a parent edge without knowing the
        // spawner's generation: presence + lease still guard the edge.
        let me = rt(1);
        let mut f = FencingState::new();
        let mut c = cell();
        c.lineage.parent_gen_id = None;
        let ev = Evidence {
            self_row: RowRead::Ok((me, id(1))),
            parent_row: RowRead::Ok((rt(9), id(77))),
            parent_lease_expired: Some(false),
        };
        assert_eq!(f.evaluate(me, &c, &ev), Verdict::Keep);
    }

    #[test]
    fn tracker_first_sight_is_alive_and_clock_starts_then() {
        let mut t = LeaseTracker::new();
        t.observe(rt(1), 7, 45_000, 0);
        assert!(!t.is_expired(rt(1), 44_000));
        assert!(t.is_expired(rt(1), 46_000));
    }

    #[test]
    fn tracker_seq_advance_resets_staleness_same_seq_does_not() {
        let mut t = LeaseTracker::new();
        t.observe(rt(1), 1, 45_000, 0);
        t.observe(rt(1), 1, 45_000, 40_000);
        assert!(t.is_expired(rt(1), 46_000));
        t.observe(rt(1), 2, 45_000, 46_000);
        assert!(!t.is_expired(rt(1), 50_000));
        assert_eq!(t.stale_for(rt(1), 50_000), Some(4_000));
    }

    #[test]
    fn tracker_unknown_node_is_never_expired() {
        let t = LeaseTracker::new();
        assert!(!t.is_expired(rt(9), 1_000_000));
        assert_eq!(t.stale_for(rt(9), 0), None);
        assert_eq!(t.ttl_ms_of(rt(9)), None);
    }

    #[test]
    fn tracker_expiry_uses_each_nodes_declared_ttl() {
        let mut t = LeaseTracker::new();
        t.observe(rt(1), 1, 45_000, 0);
        t.observe(rt(2), 1, 90_000, 0);
        assert!(t.is_expired(rt(1), 60_000));
        assert!(!t.is_expired(rt(2), 60_000));
        assert_eq!(t.expired(60_000), Vec::from([rt(1)]));
        assert_eq!(t.ttl_ms_of(rt(2)), Some(90_000));
    }

    #[test]
    fn tracker_seq_advance_adopts_a_newly_declared_ttl() {
        let mut t = LeaseTracker::new();
        t.observe(rt(1), 1, 45_000, 0);
        t.observe(rt(1), 2, 90_000, 10_000);
        assert!(!t.is_expired(rt(1), 70_000));
        assert!(t.is_expired(rt(1), 101_000));
    }

    #[test]
    fn judge_read_ok_observes_and_compares_grace() {
        let mut t = LeaseTracker::new();
        assert_eq!(
            t.judge_read(rt(1), RowRead::Ok((7, 45_000)), Some(45_000), 0),
            Some(false)
        );
        // Same seq: staleness accrues past the grace.
        assert_eq!(
            t.judge_read(rt(1), RowRead::Ok((7, 45_000)), Some(45_000), 46_000),
            Some(true)
        );
        // An advancing seq resets the clock.
        assert_eq!(
            t.judge_read(rt(1), RowRead::Ok((8, 45_000)), Some(45_000), 47_000),
            Some(false)
        );
    }

    #[test]
    fn judge_read_defaults_grace_to_the_leases_declared_ttl() {
        let mut t = LeaseTracker::new();
        assert_eq!(
            t.judge_read(rt(1), RowRead::Ok((7, 90_000)), None, 0),
            Some(false)
        );
        // 46s silent: past the cluster-default 45s, within the declared 90s.
        assert_eq!(
            t.judge_read(rt(1), RowRead::Ok((7, 90_000)), None, 46_000),
            Some(false)
        );
        assert_eq!(
            t.judge_read(rt(1), RowRead::Ok((7, 90_000)), None, 91_000),
            Some(true)
        );
    }

    #[test]
    fn judge_read_absent_row_is_not_fenceable() {
        let mut t = LeaseTracker::new();
        t.observe(rt(1), 7, 45_000, 0);
        assert_eq!(
            t.judge_read(rt(1), RowRead::Absent, Some(45_000), 90_000),
            None
        );
    }

    #[test]
    fn judge_read_failed_is_no_evidence_but_prior_observation_ages() {
        let mut t = LeaseTracker::new();
        assert_eq!(t.judge_read(rt(9), RowRead::Failed, None, 0), Some(false));
        t.observe(rt(1), 7, 45_000, 0);
        // The stored declared ttl judges the aging observation.
        assert_eq!(
            t.judge_read(rt(1), RowRead::Failed, None, 46_000),
            Some(true)
        );
    }

    #[test]
    fn tracker_expired_lists_only_expired_and_forget_removes() {
        let mut t = LeaseTracker::new();
        t.observe(rt(1), 1, 45_000, 0);
        t.observe(rt(2), 1, 45_000, 30_000);
        assert_eq!(t.expired(50_000), Vec::from([rt(1)]));
        t.forget(rt(1));
        assert!(t.expired(50_000).is_empty());
    }
}
