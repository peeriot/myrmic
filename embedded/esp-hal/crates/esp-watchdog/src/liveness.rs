//! Task-liveness monitoring — the detection layer of the hardware watchdog
//! (SDS-FEAT-2026-HWD-001, Area A1).
//!
//! Every instrumented task bumps a per-task progress counter at the top of its
//! main loop. A feeder task on the main embassy executor periodically
//! verifies that every *required* counter advanced recently and only then
//! invokes the watchdog feed hook ([`crate::watchdog::feed`], which feeds the
//! on-die RWDT/MWDT).
//!
//! Naming: the SDS (SDS-FEAT-2026-HWD-001) calls this component the *liveness
//! supervisor*; the code says *watchdog feeder* so it cannot be confused with
//! cell supervision (leases, incarnations, fencing).
//!
//! Failure semantics (SDS Area A1) — all four end in a withheld feed, so the
//! watchdog fires once #1012 arms it:
//! - a non-yielding task wedges the (cooperative) main executor → the
//!   feeder never runs → no feed;
//! - a higher-priority thread (BLE/WiFi radio) monopolizes the CPU → the
//!   whole prio-1 executor thread starves → no feed;
//! - a required task stalls while the executor stays healthy → its counter
//!   stops advancing → the feeder withholds the feed;
//! - the feeder itself wedges → no feed.
//!
//! Tasks are either **required** (must advance within their staleness
//! allowance: the node is broken if they stall) or **observed** (counter
//! recorded for diagnostics only — tasks whose progress legitimately blocks
//! for unbounded time must not gate the feed, or the node would spuriously
//! reset). Only a task with a network-independent, bounded iteration period
//! makes a good *liveness* signal; a task whose loop can park on a network
//! round-trip conflates a genuine hang with a slow or absent link.
//!
//! A required task that has **never** bumped is exempt (staleness applies from
//! the first observed bump onwards): a task that has not started yet — e.g.
//! before WiFi is up — must not trip the watchdog, since a reset would not fix
//! that.
//!
//! All periods are platform-defined (SDS: no operator configuration surface).
//! The #1014 characterization measured worst-case stalls under sustained
//! BLE/WiFi radio load: a held BLE connection perturbs the executor by only
//! ~2 ms (round gap 5002 ms vs the 5 s ideal) and leaves `stats` staleness at
//! ~25 s — well inside the 90 s allowance.

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_time::{Duration, Instant, Ticker, Timer};

/// Instrumented tasks. The discriminant indexes the per-task counters.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Task {
    /// `db_client::service` — the node's workload pump (deployment polling,
    /// mailbox, exec registration). **Observed, not required**: its loop can
    /// park on an unbounded network round-trip (the deployment poll parks
    /// during radio starvation), so its progress tracks link health, not task
    /// liveness — gating the watchdog on it false-positives on a starved or
    /// slow link (#1014). The poll→DB-subscription refactor will also remove
    /// the guaranteed loop iteration entirely (Lakier15, PR #1081).
    DbClient = 0,
    /// The `stats` heartbeat (30 s tick). **Required.**
    Stats = 1,
    /// WiFi connect/reconnect manager (observed: blocks on radio events).
    Connection = 2,
    /// Zenoh transport/reconnect loop (observed: blocks on link state).
    ZenohSession = 3,
    /// Zenoh request handler (observed: blocks on an empty request channel).
    ZenohClient = 4,
    /// Cell message pump (observed: blocks on an empty cell channel).
    Cell = 5,
    /// WASM module storage/deploy handler (observed: blocks awaiting a
    /// deployment).
    RuntimeHandler = 6,
    /// Async host-request pipeline (observed: not every request category it
    /// serializes has an audited bound yet, so its progress can legitimately
    /// track a slow DB/zenoh/BLE round-trip rather than a genuine hang).
    RequestHandler = 7,
}

const TASK_COUNT: usize = 8;

const NAMES: [&str; TASK_COUNT] = [
    "db-client",
    "stats",
    "connection",
    "zenoh-session",
    "zenoh-client",
    "cell-task",
    "runtime-handler",
    "request-handler",
];

/// Required tasks and their staleness allowance. An allowance must exceed the
/// task's worst-case legitimate iteration gap by a wide margin (no spurious
/// resets). `stats` is the sole required task: it ticks every 30 s with no
/// network dependency, so it is a clean executor-liveness proxy → 90 s is 3x.
/// (`db_client` was required until #1014 showed its network-coupled progress
/// false-positives under radio starvation; it is now observed — see [`Task`].)
pub(crate) const REQUIRED: [(Task, Duration); 1] = [(Task::Stats, Duration::from_secs(90))];

/// Supervisor round period. Also the feed cadence when the node is healthy —
/// the #1012 watchdog timeout must comfortably exceed it.
const ROUND: Duration = Duration::from_secs(5);

static COUNTERS: [AtomicU32; TASK_COUNT] = [const { AtomicU32::new(0) }; TASK_COUNT];

/// The feeder's latest verdict: bit `i` set = the required task with
/// discriminant `i` was stale in the most recent round (`0` = healthy).
/// Consumed by the watchdog stage-0 interrupt for the RTC hang record.
static STALE_MASK: AtomicU32 = AtomicU32::new(0);

/// The most recent feeder verdict as a stale-task bitmask.
pub fn stale_mask() -> u32 {
    STALE_MASK.load(Ordering::Relaxed)
}

/// Resolve a stale-task bitmask back to task names.
pub fn names_of(mask: u32) -> impl Iterator<Item = &'static str> {
    (0..TASK_COUNT).filter_map(move |i| (mask & (1 << i) != 0).then_some(NAMES[i]))
}

/// Record one iteration of `task`'s main loop. Call at the top of the loop.
///
/// Load+store instead of `fetch_add`: each counter has exactly one writer (its
/// task), and plain word load/store is atomic on riscv32imc (esp32c3, which
/// lacks the RMW instructions) as well as imac.
pub fn bump(task: Task) {
    let counter = &COUNTERS[task as usize];
    counter.store(
        counter.load(Ordering::Relaxed).wrapping_add(1),
        Ordering::Relaxed,
    );
}

/// The stats heartbeat: the sole *required* liveness task. Bumps its counter,
/// logs heap statistics, and ticks every 30 s.
///
/// `wedge_mode` (bench/HIL builds only) polls a fault-injection request each
/// second and deliberately wedges the executor (mode 1: spin) or parks this
/// required task (mode 2), driving the watchdog to reset. `stack_hwm`, when
/// given, logs the peak main-stack usage each round.
pub async fn heartbeat(wedge_mode: Option<fn() -> u8>, stack_hwm: Option<fn() -> usize>) {
    loop {
        bump(Task::Stats);

        #[cfg(feature = "wdt-selftest")]
        if let Some(mode) = wedge_mode {
            match mode() {
                1 => {
                    log::error!("[wdt-selftest] wedging executor (spin, never yields)");
                    loop {
                        core::hint::spin_loop();
                    }
                }
                2 => {
                    log::error!(
                        "[wdt-selftest] stalling `stats` (parks forever, executor stays alive)"
                    );
                    core::future::pending::<()>().await;
                }
                _ => {}
            }
        }

        let stats = esp_alloc::HEAP.stats();
        log::trace!("{stats}");

        if let Some(hwm) = stack_hwm {
            log::info!("[stack-hwm] peak main-stack usage: {} B", hwm());
        }

        // Sleep to the next tick. With a wedge hook installed, poll the trigger
        // each second so a requested wedge engages within ~1 s rather than
        // waiting up to a full 30 s tick.
        #[cfg(feature = "wdt-selftest")]
        if let Some(mode) = wedge_mode {
            for _ in 0..30 {
                // Only short-circuit the tick for the modes the match above acts
                // on; an unexpected mode must not turn the heartbeat into a busy
                // loop.
                if matches!(mode(), 1 | 2) {
                    break;
                }
                Timer::after_secs(1).await;
            }
        } else {
            Timer::after_secs(30).await;
        }
        #[cfg(not(feature = "wdt-selftest"))]
        {
            let _ = wedge_mode;
            Timer::after_secs(30).await;
        }
    }
}

/// The watchdog feeder: snapshots all counters each round and invokes
/// `feed` only when every required task advanced within its allowance.
///
/// Runs on the main embassy executor: its own scheduling *is* the executor
/// liveness signal (a wedged executor or a starved prio-1 thread stops the
/// rounds, and with them the feed).
pub async fn watchdog_feeder(feed: fn()) {
    let mut last_seen = [0u32; TASK_COUNT];
    // `None` until the task's first observed bump (see module docs).
    let mut last_change: [Option<Instant>; TASK_COUNT] = [None; TASK_COUNT];

    #[cfg(feature = "wdt-characterize")]
    let mut characterize = characterize::HighWaterMarks::new();

    let mut ticker = Ticker::every(ROUND);
    loop {
        ticker.next().await;
        let now = Instant::now();

        // The round period is the healthy feed cadence, so its worst case sizes
        // the watchdog timeout: record how late this round actually fired.
        #[cfg(feature = "wdt-characterize")]
        characterize.record_round(now);

        for i in 0..TASK_COUNT {
            let current = COUNTERS[i].load(Ordering::Relaxed);
            if current != last_seen[i] {
                last_seen[i] = current;
                last_change[i] = Some(now);
            }
        }

        let mut stale = 0u32;
        for (task, allowance) in REQUIRED {
            match last_change[task as usize] {
                // Not started yet (e.g. waiting for WiFi) — exempt.
                None => {}
                Some(at) => {
                    let age = now - at;
                    // A required task's worst-case staleness sizes its allowance.
                    #[cfg(feature = "wdt-characterize")]
                    characterize.record_age(task, age);
                    if age > allowance {
                        stale |= 1 << task as usize;
                        log::warn!(
                            "[liveness] task `{}` made no progress for {} s — withholding watchdog feed",
                            NAMES[task as usize],
                            age.as_secs(),
                        );
                    }
                }
            }
        }
        STALE_MASK.store(stale, Ordering::Relaxed);

        #[cfg(feature = "wdt-characterize")]
        characterize.log(now);

        if stale == 0 {
            feed();
        }
    }
}

/// Watchdog timeout characterization (#1014) — bench-only high-water-mark
/// tracking, owned by the feeder task. Records worst-case round lateness
/// (sizes the MWDT/RWDT timeouts) and worst-case required-task staleness
/// (sizes the per-task allowances) under real BLE/WiFi radio load.
#[cfg(feature = "wdt-characterize")]
mod characterize {
    use super::{NAMES, REQUIRED, Task};
    use embassy_time::Instant;

    pub struct HighWaterMarks {
        last_round: Instant,
        /// Worst round-to-round gap in ms (ideal = ROUND; excess is executor
        /// preemption, chiefly the radio threads).
        max_round_gap_ms: u64,
        /// Worst staleness age per REQUIRED task, same order as [`REQUIRED`].
        max_age_ms: [u64; REQUIRED.len()],
    }

    impl HighWaterMarks {
        pub fn new() -> Self {
            Self {
                last_round: Instant::now(),
                max_round_gap_ms: 0,
                max_age_ms: [0; REQUIRED.len()],
            }
        }

        pub fn record_round(&mut self, now: Instant) {
            let gap = (now - self.last_round).as_millis();
            self.last_round = now;
            self.max_round_gap_ms = self.max_round_gap_ms.max(gap);
        }

        pub fn record_age(&mut self, task: Task, age: embassy_time::Duration) {
            if let Some(slot) = REQUIRED.iter().position(|(t, _)| *t == task) {
                self.max_age_ms[slot] = self.max_age_ms[slot].max(age.as_millis());
            }
        }

        /// Log the high-water marks each round so the bench capture shows them
        /// climb and settle.
        pub fn log(&self, _now: Instant) {
            log::info!(
                "[wdt-char] max round gap {} ms (ideal 5000)",
                self.max_round_gap_ms
            );
            for (slot, (task, allowance)) in REQUIRED.iter().enumerate() {
                log::info!(
                    "[wdt-char] max staleness `{}` {} ms (allowance {} ms)",
                    NAMES[*task as usize],
                    self.max_age_ms[slot],
                    allowance.as_millis(),
                );
            }
        }
    }
}
