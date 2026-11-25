use std::time::Duration;

use claims::assert_ok;
use sorg_tests::{register_fixture_class, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

use super::mean_tick_gap_ms;

const PERIODIC_SRI: &str = "periodic_macro_cell";
const PERIODIC_CLASS: &str = "periodic_macro.wasm";
const TICK_EVENT: &str = "timer_tick";

const FIXED_DELAY_SRI: &str = "periodic_fixed_delay_cell";
const FIXED_DELAY_CLASS: &str = "periodic_fixed_delay.wasm";
const FIXED_DELAY_EVENT: &str = "fixed_delay_tick";
/// Period of the fixture's fixed-delay `interval`.
const PERIOD_MS: u64 = 200;

/// Smoke test: cell defines a periodic method with `#[periodic(every = "200ms")]`,
/// timer auto-starts in init, test receives at least one event.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn periodic_macro_emits_events() {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    register_fixture_class(
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

/// The mean gap between consecutive ticks, stamped in the cell, respects the configured period.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn periodic_macro_respects_period() -> Result<(), String> {
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    register_fixture_class(
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

    // Mean gap over 5 periods, from the stamps the cell put into the ticks
    let gap_ms = mean_tick_gap_ms(&mut event_queue, 5).await?;
    assert!(
        (100..=400).contains(&gap_ms),
        "expected mean gap ~200ms, got {gap_ms}ms"
    );

    Ok(())
}

/// With `.fixed_delay()` the next tick must not start until the previous handler returned.
///
/// The cell's timer has a 200ms period and a handler that waits 300ms; every tick publishes
/// `[entry_us, prev_exit_us]`, its own entry stamp and the previous handler's exit stamp,
/// taken as the handler's last statement. Under fixed-delay the period only starts counting
/// once the handler returned, so each tick enters at least `PERIOD_MS` after the previous
/// one exited. Under fixed-rate the ticks pile up in the inbox and the next one enters right
/// after the previous exit, a gap below `PERIOD_MS`. Slowness can only widen the gap; a stalled
/// timer trips the 10s receive timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn fixed_delay_defers_next_tick_until_handler_returns() {
    sorg_tests::enable_test_logging("info");
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    register_fixture_class(
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

    for i in 0..4 {
        let payload = tokio::time::timeout(Duration::from_secs(10), event_queue.receive())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for fixed-delay tick {i}"))
            .unwrap_or_else(|e| panic!("error receiving fixed-delay tick {i}: {e}"));
        let [entry_us, prev_exit_us]: [u64; 2] =
            serde_json::from_slice(&payload).expect("tick payload is [entry_us, prev_exit_us]");

        // The first tick has no previous exit; the fixture reports 0 for it.
        if prev_exit_us == 0 {
            assert_eq!(i, 0, "tick {i}: previous exit stamp is missing");
            continue;
        }
        let gap_us = entry_us
            .checked_sub(prev_exit_us)
            .expect("a tick must not enter before the previous one exited");
        assert!(
            gap_us >= PERIOD_MS * 1000,
            "tick {i}: entered {gap_us}us after the previous tick exited, expected at \
             least {PERIOD_MS}ms - ticks appear to be piling up (fixed-rate behaviour)"
        );
    }
}

/// Fixed-delay and fixed-rate cells running concurrently do not interfere.
///
/// Deploys a standard periodic cell (200ms fixed-rate) alongside the fixed-delay cell
/// (200ms period, 300ms handler) and asserts both continue to emit events.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn fixed_delay_does_not_starve_other_timers() {
    sorg_tests::enable_test_logging("info");
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    register_fixture_class(
        "../../tests/fixtures/cell-periodic-fixed-delay-logic",
        "periodic_fixed_delay",
        &swarm,
    )
    .await;
    register_fixture_class(
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
        tokio::time::timeout(Duration::from_secs(10), fixed_delay_events.receive())
            .await
            .expect("timed out waiting for fixed-delay event — fixed-rate cell may be starving it")
            .expect("error receiving fixed-delay event");
        tokio::time::timeout(Duration::from_secs(10), fixed_rate_events.receive())
            .await
            .expect("timed out waiting for fixed-rate event — fixed-delay cell may be starving it")
            .expect("error receiving fixed-rate event");
    }
}
