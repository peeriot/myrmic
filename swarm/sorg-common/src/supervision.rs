//! Timing knobs and the observer-local lease staleness tracker used by the
//! supervision machinery (exec fencing pass, orchestrator hygiene).

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cell_protocol::{RuntimeId, Sri};

/// Supervision timing knobs. v1 uses the defaults everywhere; runtime
/// configuration is deferred until real mesh timings are known.
#[derive(Debug, Clone, Copy)]
pub struct SupervisionTiming {
    /// Lease renewal period (R).
    pub renew: Duration,
    /// Staleness after which a lease is expired for an observer (TTL).
    pub ttl: Duration,
    /// Extra wait after expiry before the orchestrator acts (M).
    pub margin: Duration,
    /// Verification/hygiene pass period (P).
    pub verify: Duration,
}

impl SupervisionTiming {
    /// Db retention for lease renewal rows: superseded renewals are GC'd
    /// after this. Well past `ttl + margin` so the judge and hygiene always
    /// act before a dead node's last renewal is purged (5 min at defaults).
    pub fn lease_retention(&self) -> Duration {
        (self.ttl + self.margin) * 5
    }
}

impl Default for SupervisionTiming {
    fn default() -> Self {
        Self {
            renew: Duration::from_secs(10),
            ttl: Duration::from_secs(45),
            margin: Duration::from_secs(15),
            verify: Duration::from_secs(10),
        }
    }
}

/// Deterministic ±20% jitter so synchronized nodes de-synchronize their
/// renewal/verification timers without needing a real RNG in tests.
pub fn jittered(base: Duration, salt: u64) -> Duration {
    // splitmix64 finalizer; uniform enough for timer spreading.
    let mut z = salt.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let permille = 800 + (z % 401) as u32; // [800, 1200] => ±20.0%
    base * permille / 1000
}

/// Saturating `u128` -> `u64` millis (truncation is unreachable for sane
/// durations, but the tick core speaks `u64`).
fn millis_u64(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Margin over the pass age bound for the ahead direction, and the whole
/// threshold for the behind direction.
const SKEW_MARGIN_MS: i64 = 5_000;

/// Fresh same-direction confirmations required before the first report.
const CONFIRM_PASSES: u32 = 2;

/// Minimum passes between reports. One shared interval, advanced on any pass
/// where some direction holds a majority and got a fresh vote for it.
const REPEAT_PASSES: u32 = 30;

/// A peer's declared `ttl_ms` is clamped into this window before it is used
/// to retain that peer's verdict. The floor is three pass periods, so a peer
/// at the floor cannot lose its verdict between two passes.
const VERDICT_WINDOW_MIN_MS: u64 = 36_000;
const VERDICT_WINDOW_MAX_MS: u64 = 120_000;

/// Epoch millis above this are not a clock reading. Bounding the range from
/// both sides is what keeps a `u64::MAX` reading out of the arithmetic.
const PLAUSIBLE_EPOCH_MS_MAX: u64 = 4_000_000_000_000;

/// Epoch millis below this are not a clock reading. A test pins the lease
/// writer's minted `seq` inside this range; nothing enforces it at runtime.
const PLAUSIBLE_EPOCH_MS_MIN: u64 = 1_600_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Ahead,
    Behind,
    InStep,
}

/// A peer's last reading, and the margin it proved when it was measured -
/// never recomputed against a later pass's bound.
#[derive(Debug)]
struct PeerVerdict {
    direction: Direction,
    margin_ms: i64,
    expires_at: Instant,
}

#[derive(Debug)]
struct PeerState {
    seq: u64,
    verdict: Option<PeerVerdict>,
}

/// A confirmed disagreement between this node's clock and its peers'.
/// `offset_ms` is signed: positive means this node's clock reads later.
/// `peers` voted for the direction; `tracked` is the count the majority was
/// taken over.
#[derive(Debug)]
pub struct SkewReport {
    pub offset_ms: i64,
    pub peers: usize,
    pub tracked: usize,
}

/// Local clock-skew observation from the lease scan the fencing pass already
/// runs. Depends on `NodeLease.seq` being epoch millis straight off the
/// writer's wall clock.
///
/// It does not say which clock is wrong: the measurement is a difference and
/// the peer majority is the reference, so a swarm sharing one bad time source
/// is invisible. In either direction detection floors near the margin and is
/// only dependable well above it: a lease reading carries a non-negative,
/// unmeasured age, but that age varies per peer and the report takes the
/// extremum, so a row read soon after its renewal hides little of the offset.
#[derive(Debug, Default)]
pub struct ClockSkewWatch {
    peers: HashMap<RuntimeId, PeerState>,
    last_scan: Option<Instant>,
    ahead_run: u32,
    behind_run: u32,
    since_report: Option<u32>,
}

impl ClockSkewWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// This node's wall clock in epoch millis, or `None` when it reads
    /// outside the plausible range - a node with no usable clock is not one
    /// whose skew can be characterised. It is read before the lease scan, so
    /// read-path lag biases the estimate toward behind; what biases it toward
    /// ahead is a read served by a staler replica, whose rows predate the scan.
    pub fn local_now_ms() -> Option<u64> {
        let ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis(),
        )
        .ok()?;
        Self::plausible(ms).then_some(ms)
    }

    /// Whether a value is in range to be an epoch-millis clock reading.
    pub fn plausible(ms: u64) -> bool {
        (PLAUSIBLE_EPOCH_MS_MIN..=PLAUSIBLE_EPOCH_MS_MAX).contains(&ms)
    }

    /// A peer row's clock reading: `None` for this node's own row, and for a
    /// `seq` outside the plausible range - such a row is no reference at all.
    fn peer_reading(own: RuntimeId, id: RuntimeId, seq: u64) -> Option<u64> {
        (id != own && Self::plausible(seq)).then_some(seq)
    }

    /// `local - peer` in signed millis, saturating at `-i64::MAX` rather than
    /// `i64::MIN`, whose negation and absolute value both overflow.
    fn signed_diff_ms(local: u64, peer: u64) -> i64 {
        if local >= peer {
            i64::try_from(local - peer).unwrap_or(i64::MAX)
        } else {
            -i64::try_from(peer - local).unwrap_or(i64::MAX)
        }
    }

    fn margins(&self, dir: Direction) -> impl Iterator<Item = i64> {
        self.peers.values().filter_map(move |peer| {
            peer.verdict
                .as_ref()
                .filter(|v| v.direction == dir)
                .map(|v| v.margin_ms)
        })
    }

    /// Folds one lease scan into the peer verdicts and returns a report when
    /// a direction has held a majority for long enough.
    pub fn sample<I>(
        &mut self,
        own: RuntimeId,
        local_now_ms: Option<u64>,
        now: Instant,
        rows: I,
    ) -> Option<SkewReport>
    where
        I: IntoIterator<Item = (RuntimeId, u64, u64)>,
    {
        let bound = self.last_scan.map(|prev| {
            i64::try_from(now.saturating_duration_since(prev).as_millis()).unwrap_or(i64::MAX)
        });
        self.last_scan = Some(now);

        let mut present: HashSet<RuntimeId> = HashSet::new();
        let mut fresh_ahead = false;
        let mut fresh_behind = false;

        for (id, seq, ttl_ms) in rows {
            let Some(reading) = Self::peer_reading(own, id, seq) else {
                continue;
            };
            present.insert(id);
            let Some(peer) = self.peers.get_mut(&id) else {
                self.peers.insert(
                    id,
                    PeerState {
                        seq: reading,
                        verdict: None,
                    },
                );
                continue;
            };
            if reading <= peer.seq {
                // A lower reading re-baselines - a corrected peer clock, or a
                // read served by a staler replica. An equal one is no advance.
                peer.seq = reading;
                continue;
            }
            peer.seq = reading;
            let (Some(local), Some(bound)) = (local_now_ms, bound) else {
                continue;
            };
            let offset = Self::signed_diff_ms(local, reading);
            let (direction, margin_ms) = if offset.saturating_sub(bound) > SKEW_MARGIN_MS {
                (Direction::Ahead, offset.saturating_sub(bound))
            } else if offset < -SKEW_MARGIN_MS {
                (Direction::Behind, offset)
            } else {
                (Direction::InStep, 0)
            };
            let window =
                Duration::from_millis(ttl_ms.clamp(VERDICT_WINDOW_MIN_MS, VERDICT_WINDOW_MAX_MS));
            peer.verdict = Some(PeerVerdict {
                direction,
                margin_ms,
                expires_at: now.checked_add(window).unwrap_or(now),
            });
            match direction {
                Direction::Ahead => fresh_ahead = true,
                Direction::Behind => fresh_behind = true,
                Direction::InStep => {}
            }
        }

        self.peers.retain(|id, peer| {
            if peer.verdict.as_ref().is_some_and(|v| v.expires_at <= now) {
                peer.verdict = None;
            }
            peer.verdict.is_some() || present.contains(id)
        });

        self.decide(fresh_ahead, fresh_behind)
    }

    /// A direction's confirmation run. It resets when the majority is lost,
    /// advances only on a fresh vote for it, and otherwise holds.
    fn step(run: u32, majority: bool, fresh: bool) -> u32 {
        if !majority {
            0
        } else if fresh {
            run.saturating_add(1)
        } else {
            run
        }
    }

    fn decide(&mut self, fresh_ahead: bool, fresh_behind: bool) -> Option<SkewReport> {
        let tracked = self.peers.values().filter(|p| p.verdict.is_some()).count();
        let ahead_votes = self.margins(Direction::Ahead).count();
        let behind_votes = self.margins(Direction::Behind).count();
        let ahead_majority = ahead_votes.saturating_mul(2) > tracked;
        let behind_majority = behind_votes.saturating_mul(2) > tracked;

        self.ahead_run = Self::step(self.ahead_run, ahead_majority, fresh_ahead);
        self.behind_run = Self::step(self.behind_run, behind_majority, fresh_behind);

        if (ahead_majority && fresh_ahead) || (behind_majority && fresh_behind) {
            self.since_report = self.since_report.map(|n| n.saturating_add(1));
        }

        let winner = if ahead_majority && fresh_ahead && self.ahead_run >= CONFIRM_PASSES {
            self.margins(Direction::Ahead)
                .max()
                .map(|m| (ahead_votes, m))
        } else if behind_majority && fresh_behind && self.behind_run >= CONFIRM_PASSES {
            self.margins(Direction::Behind)
                .min()
                .map(|m| (behind_votes, m))
        } else {
            None
        };
        let (votes, margin) = winner?;
        let due = self.since_report.is_none_or(|n| n >= REPEAT_PASSES);
        if !due {
            return None;
        }
        self.since_report = Some(0);
        Some(SkewReport {
            offset_ms: margin,
            peers: votes,
            tracked,
        })
    }
}

/// Observer-local lease staleness: an `Instant` façade over the shared
/// tick-based core in [`cell_protocol::supervision`] (the embedded host runs
/// the same core on its SoC tick). Expiry is measured on the observer's own
/// monotonic clock from the last seq *advance* it saw; wall clocks and row
/// timestamps are never compared. First sight counts as an advance, so a
/// cold-started observer errs late, never early.
#[derive(Debug)]
pub struct LeaseTracker {
    origin: Instant,
    inner: cell_protocol::supervision::LeaseTracker,
}

impl Default for LeaseTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaseTracker {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
            inner: cell_protocol::supervision::LeaseTracker::new(),
        }
    }

    fn ms(&self, at: Instant) -> u64 {
        millis_u64(at.saturating_duration_since(self.origin))
    }

    pub fn observe(&mut self, id: RuntimeId, seq: u64, ttl: Duration, now: Instant) {
        self.inner.observe(id, seq, millis_u64(ttl), self.ms(now));
    }

    /// An unknown node is never expired: absence of lease evidence means
    /// "not fenceable", not "dead".
    pub fn is_expired(&self, id: RuntimeId, now: Instant) -> bool {
        self.inner.is_expired(id, self.ms(now))
    }

    /// How long since this observer last saw the node's lease advance;
    /// `None` for nodes it has never observed. Lets callers apply a
    /// per-edge tolerance instead of the node's declared ttl.
    pub fn stale_for(&self, id: RuntimeId, now: Instant) -> Option<Duration> {
        self.inner
            .stale_for(id, self.ms(now))
            .map(Duration::from_millis)
    }

    /// The ttl an observed node declared in its last advancing lease.
    pub fn ttl_of(&self, id: RuntimeId) -> Option<Duration> {
        self.inner.ttl_ms_of(id).map(Duration::from_millis)
    }

    pub fn expired(&self, now: Instant) -> Vec<RuntimeId> {
        self.inner.expired(self.ms(now))
    }

    pub fn forget(&mut self, id: RuntimeId) {
        self.inner.forget(id);
    }
}

/// Tracks how long nodes have been expired, releasing them only after the
/// margin (M) has passed — the grace covering observer skew and replication
/// lag before hygiene may act. A node that revives resets its gate.
#[derive(Debug)]
pub struct ExpiryGate {
    margin: Duration,
    first_seen: HashMap<RuntimeId, Instant>,
}

impl ExpiryGate {
    pub fn new(margin: Duration) -> Self {
        Self {
            margin,
            first_seen: HashMap::new(),
        }
    }

    /// Feed the CURRENT expired/absent set each pass; returns how long each
    /// member has been in it, measured from this observer's first sighting.
    /// A node that leaves the set (revives) resets its clock.
    pub fn silences(&mut self, current: &[RuntimeId], now: Instant) -> Vec<(RuntimeId, Duration)> {
        self.first_seen.retain(|id, _| current.contains(id));
        current
            .iter()
            .map(|id| {
                let at = self.first_seen.entry(*id).or_insert(now);
                (*id, now.duration_since(*at))
            })
            .collect()
    }

    /// Feed the CURRENT expired set each pass; returns those expired for
    /// longer than the margin.
    pub fn ready(&mut self, expired_now: &[RuntimeId], now: Instant) -> Vec<RuntimeId> {
        self.silences(expired_now, now)
            .into_iter()
            .filter(|(_, silent)| *silent >= self.margin)
            .map(|(id, _)| id)
            .collect()
    }
}

/// Crash-loop budget for restarting roots: a per-SRI sliding window of recent
/// restart attempts. Each root carries its own `max`/`window` in its policy, so
/// those are supplied per call rather than fixed at construction. Counters live
/// only in the leader's memory — a leader failover resets them, which the
/// persisted spec tolerates.
#[derive(Debug, Default)]
pub struct RestartBudget {
    attempts: HashMap<Sri, VecDeque<Instant>>,
}

impl RestartBudget {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a restart attempt for `sri` at `now` and returns whether it is
    /// within the root's budget (`max` attempts per `window`). Attempts older
    /// than `window` are pruned first.
    pub fn allow(&mut self, sri: Sri, max: u32, window: Duration, now: Instant) -> bool {
        let recent = self.attempts.entry(sri).or_default();
        while let Some(oldest) = recent.front() {
            if now.duration_since(*oldest) > window {
                recent.pop_front();
            } else {
                break;
            }
        }
        if recent.len() >= max as usize {
            return false;
        }
        recent.push_back(now);
        true
    }

    /// Whether the fixed inter-attempt `delay` has elapsed since this root's
    /// most recent restart attempt. True when there is no prior attempt, so the
    /// first restart after a death is not held back.
    pub fn ready(&self, sri: &Sri, delay: Duration, now: Instant) -> bool {
        self.attempts
            .get(sri)
            .and_then(|recent| recent.back())
            .is_none_or(|last| now.duration_since(*last) >= delay)
    }

    /// Uncharges the most recent attempt recorded for `sri`. For a restart
    /// that failed at placement — no eligible runtime exists yet — rather
    /// than by crashing: such an attempt says nothing about a crash loop and
    /// must not eat into the budget while the swarm waits for a runtime.
    pub fn refund(&mut self, sri: &Sri) {
        if let Some(recent) = self.attempts.get_mut(sri) {
            recent.pop_back();
            if recent.is_empty() {
                self.attempts.remove(sri);
            }
        }
    }

    /// Forgets a root's restart history (on terminal removal or give-up).
    pub fn forget(&mut self, sri: &Sri) {
        self.attempts.remove(sri);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(n: u8) -> RuntimeId {
        zenoh_protocol::core::ZenohIdProto::try_from(&[n; 8][..])
            .unwrap()
            .into()
    }

    fn sri(name: &str) -> Sri {
        Sri::of_path(name).unwrap()
    }

    #[test]
    fn budget_allows_up_to_max_then_denies() {
        let mut b = RestartBudget::new();
        let (max, win) = (3, Duration::from_mins(1));
        let t0 = Instant::now();
        let s = sri("root-a");
        assert!(b.allow(s, max, win, t0));
        assert!(b.allow(s, max, win, t0 + Duration::from_secs(1)));
        assert!(b.allow(s, max, win, t0 + Duration::from_secs(2)));
        // The 4th attempt inside the window exceeds the budget.
        assert!(!b.allow(s, max, win, t0 + Duration::from_secs(3)));
    }

    #[test]
    fn budget_prunes_attempts_outside_window() {
        let mut b = RestartBudget::new();
        let (max, win) = (2, Duration::from_mins(1));
        let t0 = Instant::now();
        let s = sri("root-a");
        assert!(b.allow(s, max, win, t0));
        assert!(b.allow(s, max, win, t0 + Duration::from_secs(1)));
        assert!(!b.allow(s, max, win, t0 + Duration::from_secs(2)));
        // Past the window the earliest attempts age out and budget frees up.
        assert!(b.allow(s, max, win, t0 + Duration::from_secs(62)));
    }

    #[test]
    fn budget_is_tracked_per_sri() {
        let mut b = RestartBudget::new();
        let (max, win) = (1, Duration::from_mins(1));
        let t0 = Instant::now();
        assert!(b.allow(sri("root-a"), max, win, t0));
        assert!(!b.allow(sri("root-a"), max, win, t0 + Duration::from_secs(1)));
        // A different root has its own independent budget.
        assert!(b.allow(sri("root-b"), max, win, t0 + Duration::from_secs(1)));
    }

    #[test]
    fn budget_ready_enforces_delay_between_attempts() {
        let mut b = RestartBudget::new();
        let t0 = Instant::now();
        let s = sri("root-a");
        let delay = Duration::from_secs(5);
        // No prior attempt: ready immediately.
        assert!(b.ready(&s, delay, t0));
        b.allow(s, 10, Duration::from_mins(1), t0);
        // Within the delay after the last attempt: not ready.
        assert!(!b.ready(&s, delay, t0 + Duration::from_secs(4)));
        // After the delay: ready again.
        assert!(b.ready(&s, delay, t0 + Duration::from_secs(5)));
    }

    #[test]
    fn refund_uncharges_only_the_latest_attempt() {
        let mut b = RestartBudget::new();
        let (max, win) = (2, Duration::from_mins(1));
        let t0 = Instant::now();
        let s = sri("root-a");
        assert!(b.allow(s, max, win, t0));
        assert!(b.allow(s, max, win, t0 + Duration::from_secs(1)));
        // Budget exhausted...
        assert!(!b.allow(s, max, win, t0 + Duration::from_secs(2)));
        // ...until the latest attempt is refunded; one attempt is still charged.
        b.refund(&s);
        assert!(b.allow(s, max, win, t0 + Duration::from_secs(3)));
        assert!(!b.allow(s, max, win, t0 + Duration::from_secs(4)));
        // Refunding an unknown root is a no-op.
        b.refund(&sri("root-b"));
        // A refunded attempt no longer holds the inter-attempt delay either.
        let mut c = RestartBudget::new();
        assert!(c.allow(s, max, win, t0));
        c.refund(&s);
        assert!(c.ready(&s, Duration::from_secs(30), t0));
    }

    #[test]
    fn forget_resets_budget() {
        let mut b = RestartBudget::new();
        let (max, win) = (1, Duration::from_mins(1));
        let t0 = Instant::now();
        let s = sri("root-a");
        assert!(b.allow(s, max, win, t0));
        assert!(!b.allow(s, max, win, t0 + Duration::from_secs(1)));
        b.forget(&s);
        assert!(b.allow(s, max, win, t0 + Duration::from_secs(2)));
    }

    const TTL: Duration = Duration::from_secs(45);

    #[test]
    fn first_sight_is_alive_and_clock_starts_then() {
        let mut t = LeaseTracker::new();
        let t0 = Instant::now();
        t.observe(rid(1), 7, TTL, t0);
        assert!(!t.is_expired(rid(1), t0 + Duration::from_secs(44)));
        assert!(t.is_expired(rid(1), t0 + Duration::from_secs(46)));
    }

    #[test]
    fn seq_advance_resets_staleness_same_seq_does_not() {
        let mut t = LeaseTracker::new();
        let t0 = Instant::now();
        t.observe(rid(1), 1, TTL, t0);
        t.observe(rid(1), 1, TTL, t0 + Duration::from_secs(40));
        assert!(t.is_expired(rid(1), t0 + Duration::from_secs(46)));
        t.observe(rid(1), 2, TTL, t0 + Duration::from_secs(46));
        assert!(!t.is_expired(rid(1), t0 + Duration::from_secs(50)));
    }

    #[test]
    fn unknown_node_is_never_expired() {
        let t = LeaseTracker::new();
        assert!(!t.is_expired(rid(9), Instant::now()));
        assert_eq!(t.ttl_of(rid(9)), None);
    }

    #[test]
    fn expiry_and_ttl_follow_each_nodes_declared_ttl() {
        let mut t = LeaseTracker::new();
        let t0 = Instant::now();
        t.observe(rid(1), 1, TTL, t0);
        t.observe(rid(2), 1, Duration::from_secs(90), t0);
        let late = t0 + Duration::from_mins(1);
        assert_eq!(t.expired(late), vec![rid(1)]);
        assert_eq!(t.ttl_of(rid(2)), Some(Duration::from_secs(90)));
    }

    #[test]
    fn expired_lists_only_expired_and_forget_removes() {
        let mut t = LeaseTracker::new();
        let t0 = Instant::now();
        t.observe(rid(1), 1, TTL, t0);
        t.observe(rid(2), 1, TTL, t0 + Duration::from_secs(30));
        let late = t0 + Duration::from_secs(50);
        assert_eq!(t.expired(late), vec![rid(1)]);
        t.forget(rid(1));
        assert!(t.expired(late).is_empty());
    }

    #[test]
    fn jitter_stays_within_20_percent_and_is_deterministic() {
        let base = Duration::from_secs(10);
        for salt in 0..100u64 {
            let j = jittered(base, salt);
            assert!(j >= Duration::from_secs(8) && j <= Duration::from_secs(12));
            assert_eq!(j, jittered(base, salt));
        }
    }

    #[test]
    fn gate_not_ready_before_margin_ready_after() {
        let mut g = ExpiryGate::new(Duration::from_secs(15));
        let t0 = Instant::now();
        assert!(g.ready(&[rid(1)], t0).is_empty());
        assert!(g.ready(&[rid(1)], t0 + Duration::from_secs(14)).is_empty());
        assert_eq!(
            g.ready(&[rid(1)], t0 + Duration::from_secs(15)),
            vec![rid(1)]
        );
    }

    #[test]
    fn silences_report_duration_and_reset_on_revival() {
        let mut g = ExpiryGate::new(Duration::from_secs(15));
        let t0 = Instant::now();
        assert_eq!(g.silences(&[rid(1)], t0), vec![(rid(1), Duration::ZERO)]);
        assert_eq!(
            g.silences(&[rid(1)], t0 + Duration::from_secs(10)),
            vec![(rid(1), Duration::from_secs(10))]
        );
        assert!(g.silences(&[], t0 + Duration::from_secs(11)).is_empty());
        assert_eq!(
            g.silences(&[rid(1)], t0 + Duration::from_secs(20)),
            vec![(rid(1), Duration::ZERO)]
        );
    }

    #[test]
    fn gate_revival_resets_the_clock() {
        let mut g = ExpiryGate::new(Duration::from_secs(15));
        let t0 = Instant::now();
        assert!(g.ready(&[rid(1)], t0).is_empty());
        // Node revives (absent from the expired set), then expires again.
        assert!(g.ready(&[], t0 + Duration::from_secs(10)).is_empty());
        assert!(g.ready(&[rid(1)], t0 + Duration::from_secs(20)).is_empty());
        assert_eq!(
            g.ready(&[rid(1)], t0 + Duration::from_secs(35)),
            vec![rid(1)]
        );
    }

    #[test]
    fn first_sight_of_a_lease_is_not_evidence() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        // A lingering row from a long-dead node, on first sight and then
        // never advancing.
        assert!(pass(&mut w, t0, 0, &[(rid(1), ahead_seq(0))]).is_none());
        assert!(pass(&mut w, t0, 1, &[(rid(1), ahead_seq(0))]).is_none());
    }

    #[test]
    fn a_swarm_of_one_is_silent() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        for n in 0..5 {
            assert!(pass(&mut w, t0, n, &[(rid(OWN), ahead_seq(n))]).is_none());
        }
    }

    #[test]
    fn millisecond_conversion_and_the_own_id_filter() {
        assert_eq!(ClockSkewWatch::signed_diff_ms(10, 4), 6);
        assert_eq!(ClockSkewWatch::signed_diff_ms(4, 10), -6);
        assert_eq!(ClockSkewWatch::signed_diff_ms(u64::MAX, 0), i64::MAX);
        // Negatable, unlike `i64::MIN`.
        assert_eq!(ClockSkewWatch::signed_diff_ms(0, u64::MAX), -i64::MAX);

        assert_eq!(
            ClockSkewWatch::peer_reading(rid(OWN), rid(OWN), T0_MS),
            None
        );
        assert_eq!(
            ClockSkewWatch::peer_reading(rid(OWN), rid(1), T0_MS),
            Some(T0_MS)
        );
        assert_eq!(ClockSkewWatch::peer_reading(rid(OWN), rid(1), 0), None);
        assert_eq!(
            ClockSkewWatch::peer_reading(rid(OWN), rid(1), u64::MAX),
            None
        );
    }

    #[test]
    fn a_retained_majority_with_a_contrary_fresh_vote_does_not_confirm() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        // Two peers read ahead, one reads in-step.
        let rows = |n: u64| {
            [
                (rid(1), ahead_seq(n)),
                (rid(2), ahead_seq(n)),
                (rid(3), in_step_seq(n)),
            ]
        };
        assert!(pass(&mut w, t0, 0, &rows(0)).is_none());
        assert!(pass(&mut w, t0, 1, &rows(1)).is_none());
        // The two ahead peers stop advancing; only the in-step peer is fresh.
        // Their retained verdicts still carry the majority, and that alone
        // must not complete the run.
        assert!(
            pass(
                &mut w,
                t0,
                2,
                &[
                    (rid(1), ahead_seq(1)),
                    (rid(2), ahead_seq(1)),
                    (rid(3), in_step_seq(2)),
                ]
            )
            .is_none()
        );
    }

    #[test]
    fn a_corrected_clock_stops_repeating_while_stale_verdicts_live() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let mut reported = Vec::new();
        for n in 0..=31 {
            let rows = [
                (rid(1), ahead_seq(n)),
                (rid(2), ahead_seq(n)),
                (rid(3), in_step_seq(n)),
            ];
            if pass(&mut w, t0, n, &rows).is_some() {
                reported.push(n);
            }
        }
        assert_eq!(reported, vec![2]);
        // The clock is corrected: the peer that is still renewing now reads
        // in-step. The two ahead verdicts are retained and still live, so the
        // majority survives - but the scheduled repeat must not fire on it.
        assert!(
            pass(
                &mut w,
                t0,
                32,
                &[
                    (rid(1), ahead_seq(31)),
                    (rid(2), ahead_seq(31)),
                    (rid(3), in_step_seq(32)),
                ]
            )
            .is_none()
        );
    }

    #[test]
    fn two_peers_declaring_different_ttls_share_one_age_bound() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let rows = |n: u64| {
            [
                (rid(1), ahead_seq(n), VERDICT_WINDOW_MIN_MS),
                (rid(2), ahead_seq(n), VERDICT_WINDOW_MAX_MS),
            ]
        };
        assert!(pass_with(&mut w, t0, 0, &rows(0)).is_none());
        assert!(pass_with(&mut w, t0, 1, &rows(1)).is_none());
        let report = pass_with(&mut w, t0, 2, &rows(2)).expect("a report");
        // Both peers cast the same vote for the same offset despite the
        // ttl they each declared.
        assert_eq!(report.peers, 2);
        assert_eq!(report.tracked, 2);
    }

    #[test]
    fn a_longer_pass_widens_the_bound_and_drops_a_borderline_vote() {
        // On a normal pass the borderline offset clears the bound plus the
        // margin and votes ahead.
        let mut normal = ClockSkewWatch::new();
        let t0 = Instant::now();
        for n in 0..2 {
            assert!(pass(&mut normal, t0, n, &[(rid(1), borderline_ahead_seq(n))]).is_none());
        }
        let report =
            pass(&mut normal, t0, 2, &[(rid(1), borderline_ahead_seq(2))]).expect("a report");
        assert_eq!(report.offset_ms, 8_000);

        // The same offsets on passes twice as far apart no longer clear it.
        let mut slow = ClockSkewWatch::new();
        for n in [0, 2, 4, 6] {
            assert!(pass(&mut slow, t0, n, &[(rid(1), borderline_ahead_seq(n))]).is_none());
        }
    }

    #[test]
    fn one_dissenting_peer_of_five_never_carries_a_majority() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        for n in 0..8 {
            let rows = [
                (rid(1), ahead_seq(n)),
                (rid(2), in_step_seq(n)),
                (rid(3), in_step_seq(n)),
                (rid(4), in_step_seq(n)),
                (rid(5), in_step_seq(n)),
            ];
            assert!(pass(&mut w, t0, n, &rows).is_none());
        }
    }

    #[test]
    fn two_of_three_peers_agreeing_reports() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let rows = |n: u64| {
            [
                (rid(1), ahead_seq(n)),
                (rid(2), ahead_seq(n)),
                (rid(3), in_step_seq(n)),
            ]
        };
        assert!(pass(&mut w, t0, 0, &rows(0)).is_none());
        assert!(pass(&mut w, t0, 1, &rows(1)).is_none());
        let report = pass(&mut w, t0, 2, &rows(2)).expect("a report");
        assert_eq!(report.offset_ms, 10_000);
        assert_eq!(report.peers, 2);
        assert_eq!(report.tracked, 3);
    }

    #[test]
    fn four_of_five_peers_agreeing_reports() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let rows = |n: u64| {
            [
                (rid(1), ahead_seq(n)),
                (rid(2), ahead_seq(n)),
                (rid(3), ahead_seq(n)),
                (rid(4), ahead_seq(n)),
                (rid(5), in_step_seq(n)),
            ]
        };
        assert!(pass(&mut w, t0, 0, &rows(0)).is_none());
        assert!(pass(&mut w, t0, 1, &rows(1)).is_none());
        let report = pass(&mut w, t0, 2, &rows(2)).expect("a report");
        assert_eq!(report.offset_ms, 10_000);
        assert_eq!(report.peers, 4);
        assert_eq!(report.tracked, 5);
    }

    #[test]
    fn one_of_two_peers_disagreeing_stays_silent() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        for n in 0..6 {
            let rows = [(rid(1), ahead_seq(n)), (rid(2), in_step_seq(n))];
            assert!(pass(&mut w, t0, n, &rows).is_none());
        }
    }

    #[test]
    fn peers_split_across_opposite_directions_stay_silent() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        for n in 0..6 {
            let rows = [(rid(1), ahead_seq(n)), (rid(2), behind_seq(n))];
            assert!(pass(&mut w, t0, n, &rows).is_none());
        }
    }

    #[test]
    fn in_step_peers_count_toward_the_tracked_total() {
        let mut with_bystanders = ClockSkewWatch::new();
        let t0 = Instant::now();
        for n in 0..4 {
            let rows = [
                (rid(1), ahead_seq(n)),
                (rid(2), in_step_seq(n)),
                (rid(3), in_step_seq(n)),
                (rid(4), in_step_seq(n)),
                (rid(5), in_step_seq(n)),
            ];
            assert!(pass(&mut with_bystanders, t0, n, &rows).is_none());
        }
        // The same lone voter, with the in-step peers gone from the
        // denominator, does report.
        let mut alone = ClockSkewWatch::new();
        assert!(pass(&mut alone, t0, 0, &[(rid(1), ahead_seq(0))]).is_none());
        assert!(pass(&mut alone, t0, 1, &[(rid(1), ahead_seq(1))]).is_none());
        let report = pass(&mut alone, t0, 2, &[(rid(1), ahead_seq(2))]).expect("a report");
        assert_eq!(report.tracked, 1);
    }

    #[test]
    fn a_lingering_row_leaves_the_majority_after_its_declared_ttl() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let lingering_ttl = VERDICT_WINDOW_MIN_MS;
        assert!(
            pass_with(
                &mut w,
                t0,
                0,
                &[
                    (rid(1), in_step_seq(0), lingering_ttl),
                    (rid(2), ahead_seq(0), PEER_TTL_MS),
                ]
            )
            .is_none()
        );
        assert!(
            pass_with(
                &mut w,
                t0,
                1,
                &[
                    (rid(1), in_step_seq(1), lingering_ttl),
                    (rid(2), ahead_seq(1), PEER_TTL_MS),
                ]
            )
            .is_none()
        );
        // The lingering row stays present but never advances again.
        for n in 2..=5 {
            assert!(
                pass_with(
                    &mut w,
                    t0,
                    n,
                    &[
                        (rid(1), in_step_seq(1), lingering_ttl),
                        (rid(2), ahead_seq(n), PEER_TTL_MS),
                    ],
                )
                .is_none(),
                "pass {n} should be silent"
            );
        }
        let report = pass_with(
            &mut w,
            t0,
            6,
            &[
                (rid(1), in_step_seq(1), lingering_ttl),
                (rid(2), ahead_seq(6), PEER_TTL_MS),
            ],
        )
        .expect("a report");
        assert_eq!(report.tracked, 1);
        assert_eq!(report.peers, 1);
    }

    #[test]
    fn a_single_peer_reports_and_says_so() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        assert!(pass(&mut w, t0, 0, &[(rid(1), ahead_seq(0))]).is_none());
        assert!(pass(&mut w, t0, 1, &[(rid(1), ahead_seq(1))]).is_none());
        let report = pass(&mut w, t0, 2, &[(rid(1), ahead_seq(2))]).expect("a report");
        assert_eq!(report.peers, 1);
        assert_eq!(report.tracked, 1);
    }

    #[test]
    fn the_largest_ahead_disagreement_is_the_greatest_margin() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let rows = |n: u64| [(rid(1), ahead_seq(n)), (rid(2), far_ahead_seq(n))];
        assert!(pass(&mut w, t0, 0, &rows(0)).is_none());
        assert!(pass(&mut w, t0, 1, &rows(1)).is_none());
        let report = pass(&mut w, t0, 2, &rows(2)).expect("a report");
        assert_eq!(report.offset_ms, 30_000);
        assert_eq!(report.peers, 2);
    }

    #[test]
    fn the_largest_behind_disagreement_is_the_least_margin() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let rows = |n: u64| [(rid(1), behind_seq(n)), (rid(2), far_behind_seq(n))];
        assert!(pass(&mut w, t0, 0, &rows(0)).is_none());
        assert!(pass(&mut w, t0, 1, &rows(1)).is_none());
        let report = pass(&mut w, t0, 2, &rows(2)).expect("a report");
        assert_eq!(report.offset_ms, -30_000);
        assert_eq!(report.peers, 2);
    }

    #[test]
    fn verdicts_measured_under_different_bounds_are_not_recomputed() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        assert!(
            pass(
                &mut w,
                t0,
                0,
                &[(rid(1), far_ahead_seq(0)), (rid(2), ahead_seq(0))]
            )
            .is_none()
        );
        // One peer's verdict is measured across a double-length pass, so its
        // stored margin is 20 s rather than the 30 s a 10 s bound would give.
        assert!(
            pass(
                &mut w,
                t0,
                2,
                &[(rid(1), far_ahead_seq(2)), (rid(2), ahead_seq(0))]
            )
            .is_none()
        );
        let report = pass(
            &mut w,
            t0,
            3,
            &[(rid(1), far_ahead_seq(2)), (rid(2), ahead_seq(3))],
        )
        .expect("a report");
        assert_eq!(report.offset_ms, 20_000);
        assert_eq!(report.peers, 2);
    }

    #[test]
    fn a_reading_behind_local_time_reports_at_a_smaller_magnitude() {
        let mut behind = ClockSkewWatch::new();
        let t0 = Instant::now();
        assert!(pass(&mut behind, t0, 0, &[(rid(1), behind_seq(0))]).is_none());
        assert!(pass(&mut behind, t0, 1, &[(rid(1), behind_seq(1))]).is_none());
        let report = pass(&mut behind, t0, 2, &[(rid(1), behind_seq(2))]).expect("a report");
        assert_eq!(report.offset_ms, -10_000);

        // The same magnitude in the other direction is inside the age bound
        // and votes in-step.
        let mut ahead = ClockSkewWatch::new();
        for n in 0..6 {
            assert!(pass(&mut ahead, t0, n, &[(rid(1), mild_ahead_seq(n))]).is_none());
        }
    }

    #[test]
    fn reporting_needs_two_fresh_same_direction_confirmations() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        assert!(pass(&mut w, t0, 0, &[(rid(1), ahead_seq(0))]).is_none());
        assert!(pass(&mut w, t0, 1, &[(rid(1), ahead_seq(1))]).is_none());
        assert!(pass(&mut w, t0, 2, &[(rid(1), ahead_seq(2))]).is_some());
    }

    #[test]
    fn the_first_report_is_not_delayed_by_the_repeat_interval() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let mut reported = None;
        for n in 0..=8 {
            if pass(&mut w, t0, n, &[(rid(1), ahead_seq(n))]).is_some() {
                reported = Some(n);
                break;
            }
        }
        // Two confirmations, not the repeat interval.
        assert_eq!(reported, Some(2));
    }

    #[test]
    fn a_report_repeats_only_after_the_interval() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let mut reported = Vec::new();
        for n in 0..=40 {
            if pass(&mut w, t0, n, &[(rid(1), ahead_seq(n))]).is_some() {
                reported.push(n);
            }
        }
        assert_eq!(reported, vec![2, 32]);
    }

    #[test]
    fn a_pass_without_a_fresh_vote_neither_reports_nor_shortens_the_next() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        for n in 0..2 {
            assert!(pass(&mut w, t0, n, &[(rid(1), ahead_seq(n))]).is_none());
        }
        assert!(pass(&mut w, t0, 2, &[(rid(1), ahead_seq(2))]).is_some());
        // Two passes where the row is present but frozen: the retained
        // verdict still carries the majority and nothing is emitted.
        assert!(pass(&mut w, t0, 3, &[(rid(1), ahead_seq(2))]).is_none());
        assert!(pass(&mut w, t0, 4, &[(rid(1), ahead_seq(2))]).is_none());
        // And they did not count toward the repeat: it lands two passes later
        // than it would have.
        let mut repeat = None;
        for n in 5..=40 {
            if pass(&mut w, t0, n, &[(rid(1), ahead_seq(n))]).is_some() {
                repeat = Some(n);
                break;
            }
        }
        assert_eq!(repeat, Some(34));
    }

    #[test]
    fn an_irregular_oscillation_reports_once_not_once_per_swing() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        // Irregular, deliberately not alternating.
        let swings = [
            true, true, true, false, true, true, false, false, true, true, true, false, true, true,
            false, false, false, true, true, true,
        ];
        let mut reports = 0;
        for (i, ahead) in swings.iter().enumerate() {
            let n = u64::try_from(i).expect("pass index fits");
            let seq = if *ahead { ahead_seq(n) } else { in_step_seq(n) };
            if pass(&mut w, t0, n, &[(rid(1), seq)]).is_some() {
                reports += 1;
            }
        }
        assert_eq!(reports, 1);
    }

    #[test]
    fn a_corrected_peer_clock_rebaselines_and_votes_again() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        assert!(pass(&mut w, t0, 0, &[(rid(1), ahead_seq(0))]).is_none());
        assert!(pass(&mut w, t0, 1, &[(rid(1), ahead_seq(1))]).is_none());
        // The peer's clock is corrected: its seq steps back and keeps
        // advancing from there. The step itself is not a sample.
        let corrected = ahead_seq(0) - 50_000;
        assert!(pass(&mut w, t0, 2, &[(rid(1), corrected)]).is_none());
        assert!(pass(&mut w, t0, 3, &[(rid(1), corrected + PASS_MS)]).is_some());
    }

    #[test]
    fn a_staler_replica_read_rebaselines_without_losing_the_peer() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        assert!(pass(&mut w, t0, 0, &[(rid(1), ahead_seq(0))]).is_none());
        assert!(pass(&mut w, t0, 1, &[(rid(1), ahead_seq(1))]).is_none());
        // One pass is served by a staler replica and reads lower.
        assert!(pass(&mut w, t0, 2, &[(rid(1), ahead_seq(0))]).is_none());
        // The next fresh read is an advance again and completes the run.
        assert!(pass(&mut w, t0, 3, &[(rid(1), ahead_seq(3))]).is_some());
    }

    #[test]
    fn a_seq_below_the_plausible_range_is_not_a_reference() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let rows = |n: u64| [(rid(1), 0), (rid(2), ahead_seq(n))];
        assert!(pass(&mut w, t0, 0, &rows(0)).is_none());
        assert!(pass(&mut w, t0, 1, &rows(1)).is_none());
        let report = pass(&mut w, t0, 2, &rows(2)).expect("a report");
        // The pre-epoch row never entered the denominator.
        assert_eq!(report.tracked, 1);
        assert_eq!(report.peers, 1);
    }

    #[test]
    fn a_seq_at_the_top_of_its_range_is_not_a_reference() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let rows = |n: u64| [(rid(1), u64::MAX), (rid(2), ahead_seq(n))];
        assert!(pass(&mut w, t0, 0, &rows(0)).is_none());
        assert!(pass(&mut w, t0, 1, &rows(1)).is_none());
        let report = pass(&mut w, t0, 2, &rows(2)).expect("a report");
        assert_eq!(report.tracked, 1);
        assert_eq!(report.peers, 1);
    }

    #[test]
    fn an_implausible_declared_ttl_is_clamped() {
        // A zero ttl cannot expire a verdict inside a pass.
        let mut zero = ClockSkewWatch::new();
        let t0 = Instant::now();
        assert!(pass_with(&mut zero, t0, 0, &[(rid(1), ahead_seq(0), 0)]).is_none());
        assert!(pass_with(&mut zero, t0, 1, &[(rid(1), ahead_seq(1), 0)]).is_none());
        // Frozen row: the verdict is retained, so the run holds for want of a
        // fresh vote rather than being reset.
        assert!(pass_with(&mut zero, t0, 2, &[(rid(1), ahead_seq(1), 0)]).is_none());
        assert!(pass_with(&mut zero, t0, 3, &[(rid(1), ahead_seq(3), 0)]).is_some());

        // A maximal ttl cannot retain one past the ceiling.
        let mut forever = ClockSkewWatch::new();
        let rows = |n: u64, frozen: u64| {
            [
                (rid(1), in_step_seq(frozen), u64::MAX),
                (rid(2), ahead_seq(n), PEER_TTL_MS),
            ]
        };
        assert!(pass_with(&mut forever, t0, 0, &rows(0, 0)).is_none());
        assert!(pass_with(&mut forever, t0, 1, &rows(1, 1)).is_none());
        for n in 2..=13 {
            assert!(
                pass_with(&mut forever, t0, n, &rows(n, 1)).is_none(),
                "pass {n} should be silent"
            );
        }
        let report = pass_with(&mut forever, t0, 14, &rows(14, 1)).expect("a report");
        assert_eq!(report.tracked, 1);
    }

    #[test]
    fn an_unset_local_clock_yields_no_evidence() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        for n in 0..6 {
            let rows = [(rid(1), ahead_seq(n), PEER_TTL_MS)];
            assert!(
                w.sample(rid(OWN), None, at(t0, n), rows.iter().copied())
                    .is_none()
            );
        }
    }

    #[test]
    fn a_slow_cadence_swarm_reports_a_real_skew() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        // Two peers renewing far slower than this observer scans, staggered.
        assert!(
            pass(
                &mut w,
                t0,
                0,
                &[(rid(1), ahead_seq(0)), (rid(2), ahead_seq(0))]
            )
            .is_none()
        );
        assert!(
            pass(
                &mut w,
                t0,
                1,
                &[(rid(1), ahead_seq(1)), (rid(2), ahead_seq(0))]
            )
            .is_none()
        );
        for n in 2..=3 {
            assert!(
                pass(
                    &mut w,
                    t0,
                    n,
                    &[(rid(1), ahead_seq(1)), (rid(2), ahead_seq(0))]
                )
                .is_none(),
                "pass {n} should be silent"
            );
        }
        let report = pass(
            &mut w,
            t0,
            4,
            &[(rid(1), ahead_seq(1)), (rid(2), ahead_seq(4))],
        )
        .expect("a report");
        assert_eq!(report.peers, 2);
        assert_eq!(report.tracked, 2);
    }

    #[test]
    fn intervening_passes_without_a_fresh_vote_do_not_reset_the_run() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        let ttl = VERDICT_WINDOW_MAX_MS;
        assert!(pass_with(&mut w, t0, 0, &[(rid(1), ahead_seq(0), ttl)]).is_none());
        assert!(pass_with(&mut w, t0, 1, &[(rid(1), ahead_seq(1), ttl)]).is_none());
        // Six passes carrying no fresh vote at all.
        for n in 2..=7 {
            assert!(
                pass_with(&mut w, t0, n, &[(rid(1), ahead_seq(1), ttl)]).is_none(),
                "pass {n} should be silent"
            );
        }
        // The second advance completes the run: it was held, not reset.
        assert!(pass_with(&mut w, t0, 8, &[(rid(1), ahead_seq(8), ttl)]).is_some());
    }

    #[test]
    fn a_present_rows_record_survives_its_verdict_expiring() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        assert!(pass(&mut w, t0, 0, &[(rid(1), ahead_seq(0))]).is_none());
        assert!(pass(&mut w, t0, 1, &[(rid(1), ahead_seq(1))]).is_none());
        // Present but frozen well past the verdict window.
        for n in 2..=6 {
            assert!(
                pass(&mut w, t0, n, &[(rid(1), ahead_seq(1))]).is_none(),
                "pass {n} should be silent"
            );
        }
        // The record survived, so the next advance is a sample rather than a
        // first sight, and two of them are enough.
        assert!(pass(&mut w, t0, 7, &[(rid(1), ahead_seq(7))]).is_none());
        assert!(pass(&mut w, t0, 8, &[(rid(1), ahead_seq(8))]).is_some());
    }

    #[test]
    fn an_absent_peer_with_an_expired_verdict_returns_as_first_sight() {
        let mut w = ClockSkewWatch::new();
        let t0 = Instant::now();
        assert!(pass(&mut w, t0, 0, &[(rid(1), ahead_seq(0))]).is_none());
        assert!(pass(&mut w, t0, 1, &[(rid(1), ahead_seq(1))]).is_none());
        // Absent from the scan until its verdict has expired: the record goes
        // with it.
        for n in 2..=6 {
            assert!(
                pass(&mut w, t0, n, &[]).is_none(),
                "pass {n} should be silent"
            );
        }
        // On return it is first sight again - one pass later than the
        // present-row case.
        assert!(pass(&mut w, t0, 7, &[(rid(1), ahead_seq(7))]).is_none());
        assert!(pass(&mut w, t0, 8, &[(rid(1), ahead_seq(8))]).is_none());
        assert!(pass(&mut w, t0, 9, &[(rid(1), ahead_seq(9))]).is_some());
    }

    const OWN: u8 = 9;
    const T0_MS: u64 = 1_700_000_000_000;
    const PASS_MS: u64 = 10_000;
    const PEER_TTL_MS: u64 = 45_000;

    /// This observer's wall clock at pass `n`.
    fn local_at(n: u64) -> u64 {
        T0_MS + n * PASS_MS
    }

    /// The monotonic instant of pass `n`.
    fn at(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n * PASS_MS)
    }

    /// Readings against a 10 s pass bound and a 5 s margin: 20 s of offset
    /// votes ahead, 1 s votes in-step, 10 s of negative offset votes behind.
    fn ahead_seq(n: u64) -> u64 {
        local_at(n) - 20_000
    }

    fn far_ahead_seq(n: u64) -> u64 {
        local_at(n) - 40_000
    }

    fn borderline_ahead_seq(n: u64) -> u64 {
        local_at(n) - 18_000
    }

    fn mild_ahead_seq(n: u64) -> u64 {
        local_at(n) - 10_000
    }

    fn in_step_seq(n: u64) -> u64 {
        local_at(n) - 1_000
    }

    fn behind_seq(n: u64) -> u64 {
        local_at(n) + 10_000
    }

    fn far_behind_seq(n: u64) -> u64 {
        local_at(n) + 30_000
    }

    /// One pass, every row declaring the same ttl.
    fn pass(
        w: &mut ClockSkewWatch,
        t0: Instant,
        n: u64,
        rows: &[(RuntimeId, u64)],
    ) -> Option<SkewReport> {
        let rows: Vec<(RuntimeId, u64, u64)> = rows
            .iter()
            .map(|(id, seq)| (*id, *seq, PEER_TTL_MS))
            .collect();
        pass_with(w, t0, n, &rows)
    }

    /// One pass with a per-row ttl.
    fn pass_with(
        w: &mut ClockSkewWatch,
        t0: Instant,
        n: u64,
        rows: &[(RuntimeId, u64, u64)],
    ) -> Option<SkewReport> {
        w.sample(rid(OWN), Some(local_at(n)), at(t0, n), rows.iter().copied())
    }
}
