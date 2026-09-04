//! Timing knobs and the observer-local lease staleness tracker used by the
//! supervision machinery (exec fencing pass, orchestrator hygiene).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

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
}
