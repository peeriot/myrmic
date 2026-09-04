use std::time::{Duration, Instant};

use claims::assert_ok;
use sorg_tests::{build_and_register_cell_class, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

const PERIODIC_SRI: &str = "periodic_macro_cell";
const PERIODIC_CLASS: &str = "periodic_macro.wasm";
const TICK_EVENT: &str = "timer_tick";

const FIXED_DELAY_SRI: &str = "periodic_fixed_delay_cell";
const FIXED_DELAY_CLASS: &str = "periodic_fixed_delay.wasm";
const FIXED_DELAY_EVENT: &str = "fixed_delay_tick";
/// Handler sleeps this long to simulate slow work.
const HANDLER_SLEEP_MS: u64 = 120;
/// Period configured on the cell (`every = "50ms"`).
const PERIOD_MS: u64 = 50;

/// Smoke test: cell defines a periodic method with `#[periodic(every = "200ms")]`,
/// timer auto-starts in init, test receives at least one event.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn periodic_macro_emits_events() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-periodic-macro-logic",
        "periodic_macro",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell("periodic_macro.wasm".to_owned(), PERIODIC_SRI.to_owned())
        .await;

    let received = assert_ok!(
        tokio::time::timeout(Duration::from_secs(2), event_queue.receive())
            .await
            .expect("timed out waiting for tick event — tick export may not be registered")
    );
    assert!(!received.is_empty(), "tick event should have a payload");
}

/// The gap between consecutive events respects the configured period.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn periodic_macro_respects_period() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-periodic-macro-logic",
        "periodic_macro",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell("periodic_macro.wasm".to_owned(), PERIODIC_SRI.to_owned())
        .await;

    // Drain all events during the setup
    assert_ok!(
        tokio::time::timeout(Duration::from_secs(2), event_queue.drain())
            .await
            .expect("timed out waiting for first tick event — tick export may not be registered")
    );

    // Measure 3 consecutive gaps and assert each falls within tolerance
    for _ in 0..3 {
        let t0 = Instant::now();
        assert_ok!(
            tokio::time::timeout(Duration::from_secs(2), event_queue.receive())
                .await
                .expect("timed out waiting for tick event")
        );
        let gap = t0.elapsed();
        assert!(
            gap > Duration::from_millis(100) && gap < Duration::from_millis(400),
            "expected gap ~200ms, got {gap:?}"
        );
    }
}

/// With `wait_until_finished = true` the next tick must not fire until the handler returns.
///
/// The cell is configured `every = "50ms"` but the handler sleeps 120ms. Under fixed-rate
/// scheduling ticks would queue up and events would arrive in rapid bursts (~50ms apart).
/// Under fixed-delay the gap between consecutive events must be at least as long as the
/// handler itself (~120ms), because the 50ms period only starts counting after the handler
/// returns.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn fixed_delay_defers_next_tick_until_handler_returns() {
    sorg_tests::enable_test_logging("info");
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-periodic-fixed-delay-logic",
        "periodic_fixed_delay",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(FIXED_DELAY_EVENT).await;

    test_app
        .deploy_wasm_cell(FIXED_DELAY_CLASS.to_owned(), FIXED_DELAY_SRI.to_owned())
        .await;

    // Discard the first event — it may include cold-start overhead.
    tokio::time::timeout(Duration::from_secs(5), event_queue.receive())
        .await
        .expect("timed out waiting for first fixed-delay tick")
        .expect("error receiving first fixed-delay tick");

    // Measure gaps between three consecutive events.
    for i in 0..3 {
        let t0 = Instant::now();
        tokio::time::timeout(Duration::from_secs(5), event_queue.receive())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for fixed-delay tick {i}"))
            .unwrap_or_else(|e| panic!("error receiving fixed-delay tick {i}: {e}"));
        let gap = t0.elapsed();

        // The gap must be at least as long as the handler sleep, proving the next tick was
        // held back until the previous invocation finished.
        assert!(
            gap >= Duration::from_millis(HANDLER_SLEEP_MS - 20),
            "tick {i}: gap {gap:?} is shorter than the handler duration ({HANDLER_SLEEP_MS}ms) \
             — ticks appear to be piling up (fixed-rate behaviour)"
        );

        // Sanity upper bound: the timer must not have stalled entirely.
        let max_gap = Duration::from_millis(HANDLER_SLEEP_MS + PERIOD_MS + 300);
        assert!(
            gap < max_gap,
            "tick {i}: gap {gap:?} exceeds {max_gap:?} — timer may have stalled"
        );
    }
}

/// Fixed-delay and fixed-rate cells running concurrently do not interfere.
///
/// Deploys a standard periodic cell (200ms fixed-rate) alongside the fixed-delay cell
/// (50ms period, 120ms handler) and asserts both continue to emit events.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn fixed_delay_does_not_starve_other_timers() {
    sorg_tests::enable_test_logging("info");
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-periodic-fixed-delay-logic",
        "periodic_fixed_delay",
        &swarm,
    )
    .await;
    build_and_register_cell_class(
        "../../tests/fixtures/cell-periodic-macro-logic",
        "periodic_macro",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut fixed_delay_events = test_app.subscribe_cell_event(FIXED_DELAY_EVENT).await;
    let mut fixed_rate_events = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell(FIXED_DELAY_CLASS.to_owned(), FIXED_DELAY_SRI.to_owned())
        .await;
    test_app
        .deploy_wasm_cell(PERIODIC_CLASS.to_owned(), PERIODIC_SRI.to_owned())
        .await;

    // Both cells must emit at least two events each within a generous window.
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(5), fixed_delay_events.receive())
            .await
            .expect("timed out waiting for fixed-delay event — fixed-rate cell may be starving it")
            .expect("error receiving fixed-delay event");
        tokio::time::timeout(Duration::from_secs(5), fixed_rate_events.receive())
            .await
            .expect("timed out waiting for fixed-rate event — fixed-delay cell may be starving it")
            .expect("error receiving fixed-rate event");
    }
}
