//! Fenced time seam: milliseconds since process (or test) start.
//!
//! This is the ONLY site in the codebase that calls `tokio::time::Instant::now`
//! directly.  All other code must call `time::now_millis()` instead.
//! The scoped `#[allow]` below is intentional; the workspace `clippy.toml` bans
//! the direct call everywhere else.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::Instant;

/// A fixed reference instant captured once at process start (or on the first
/// call into this module).  It is never mutated after initialisation, so
/// `OnceLock` is the correct primitive — no unsafe required.
///
/// Every tokio `Instant` is measured as microseconds elapsed since this
/// reference, giving a stable `u64` representation.
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// The anchor in microseconds from `EPOCH`.
///
/// `u64::MAX` is the sentinel meaning "not yet set"; a well-behaved process
/// will never run for 584 942 years, so the sentinel never collides with a
/// real value.
///
/// Using an `AtomicU64` makes the test reset a single safe atomic store —
/// no unsafe pointer manipulation required.
const UNSET: u64 = u64::MAX;
static ANCHOR_US: AtomicU64 = AtomicU64::new(UNSET);

/// Return microseconds elapsed since `EPOCH` for the current tokio clock value.
#[inline]
#[allow(clippy::disallowed_methods)]
fn now_us() -> u64 {
    let epoch = *EPOCH.get_or_init(Instant::now);
    #[allow(clippy::disallowed_methods)]
    let elapsed = Instant::now().duration_since(epoch);
    // Truncation: u64::MAX µs ≈ 584 942 years — safe.
    #[allow(clippy::cast_possible_truncation)]
    let us = elapsed.as_micros() as u64;
    us
}

/// Returns the number of milliseconds elapsed since the anchor was set.
///
/// The anchor is pinned on the first call (process-start semantics, D6).
/// Under `tokio::time::pause()` this advances only when the test advances
/// the mock clock, making tests fully deterministic.
pub fn now_millis() -> u64 {
    let anchor = match ANCHOR_US.load(Ordering::Acquire) {
        UNSET => {
            // First call: try to pin the anchor.
            let sample = now_us();
            match ANCHOR_US.compare_exchange(UNSET, sample, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => sample,
                // Another thread won the race; use the winner's value.
                Err(winner) => winner,
            }
        }
        v => v,
    };

    let elapsed_us = now_us().saturating_sub(anchor);
    // µs → ms.
    elapsed_us / 1_000
}

/// Re-anchor the time seam to "now".
///
/// Intended only for tests that simulate a pipeline restart (SR-15 / D6).
/// Under `tokio::time::pause()` "now" is the paused mock clock value, so
/// calling `advance` before this makes the next `now_millis()` return a value
/// near 0 relative to the new anchor.
#[cfg(test)]
pub fn reset_for_test() {
    // Ensure EPOCH is initialised before we sample the new anchor so that
    // `now_us()` is consistent.
    let new_anchor = now_us();
    // Safe atomic store — no unsafe required.
    ANCHOR_US.store(new_anchor, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Consolidated time-seam test.
    ///
    /// All scenarios that mutate the global anchor (`reset_for_test`) are
    /// executed sequentially inside a single tokio test so that parallel test
    /// threads never race on `ANCHOR_US`.
    ///
    /// Scenarios verified (all assertions carry their original spec intent):
    ///
    ///  1. `now_millis_starts_near_zero`        — reads 0 immediately after anchor reset
    ///  2. `now_millis_monotonic_after_advance`  — delta >= 1500 ms after 1500 ms advance
    ///  3. `reset_for_test_reanchors`            — after reset, reading is < pre-reset value
    ///  4. `restart_resets_clock_to_near_zero`   — after reset, reading is exactly 0
    ///  5. `restart_clock_advances_from_zero`    — post-restart 1 s advance gives ~1000 ms
    #[tokio::test(start_paused = true)]
    async fn time_seam_sequential_scenarios() {
        // ── Scenario 1: starts_near_zero ─────────────────────────────────────
        reset_for_test();
        let t = now_millis();
        assert_eq!(t, 0, "scenario 1: expected 0 ms at start, got {t}");

        // ── Scenario 2: monotonic_after_advance ──────────────────────────────
        reset_for_test();
        let t0 = now_millis();
        tokio::time::advance(std::time::Duration::from_millis(1500)).await;
        let t1 = now_millis();
        let delta = t1.saturating_sub(t0);
        assert!(
            delta >= 1500,
            "scenario 2: expected delta >= 1500 ms after 1500 ms advance, got {delta}"
        );

        // ── Scenario 3: reset_for_test_reanchors ─────────────────────────────
        reset_for_test();
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        let before = now_millis();
        assert!(
            before >= 5000,
            "scenario 3 sanity: expected >= 5000, got {before}"
        );

        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        reset_for_test();
        let after = now_millis();
        assert!(
            after < before,
            "scenario 3: after reset_for_test, now_millis ({after}) should be < previous ({before})"
        );

        // ── Scenario 4: restart_resets_clock_to_near_zero ────────────────────
        reset_for_test();
        tokio::time::advance(std::time::Duration::from_mins(1)).await;
        let before_restart = now_millis();
        assert!(
            before_restart >= 60_000,
            "scenario 4 sanity: {before_restart} ms after 60 s advance"
        );

        reset_for_test();
        let after_restart = now_millis();
        assert_eq!(
            after_restart, 0,
            "scenario 4 SR-15/D6: now_millis must be 0 immediately after restart; got {after_restart}"
        );

        // ── Scenario 5: restart_clock_advances_from_zero_not_from_previous ───
        reset_for_test();
        tokio::time::advance(std::time::Duration::from_secs(30)).await;
        let pre_restart = now_millis();
        assert!(pre_restart >= 30_000, "scenario 5 sanity: {pre_restart} ms");

        reset_for_test();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        let post_restart_1s = now_millis();

        assert!(
            post_restart_1s >= 1000,
            "scenario 5: clock should have advanced ~1000 ms post-restart; got {post_restart_1s}"
        );
        assert!(
            post_restart_1s < pre_restart,
            "scenario 5 SR-15/D6: post-restart clock ({post_restart_1s}) must be < pre-restart value ({pre_restart})"
        );
        assert!(
            post_restart_1s < pre_restart / 2,
            "scenario 5: post-restart clock must be near zero, not near the old value"
        );
    }
}
