//! Embedded-device timer integration tests.
//!
//! Mirrors a subset of `swarm/sorg-execution/tests/integration/cells/timers.rs`
//! but runs against a real ESP32 device.

use std::time::Duration;

use claims::{assert_matches, assert_ok};

use crate::integration::{
    aot::build_aot_cell, device_present, espflash::flash_device, hil_swarm_test,
};

const TIMER_INIT_CELL: &str = "cell-timer-init-logic";
const TIMER_CMD_CELL: &str = "cell-timer-cmd-logic";
const FIXED_DELAY_CELL: &str = "cell-periodic-fixed-delay-logic";

const TIMER_INIT_SRI: &str = "emb_timer_init";
const TIMER_CMD_SRI: &str = "emb_timer_cmd";
const TICK_EVENT: &str = "timer_tick";

const FIXED_DELAY_SRI: &str = "emb_fixed_delay";
const FIXED_DELAY_TICK_A: &str = "fixed_delay_tick";
const FIXED_DELAY_TICK_B: &str = "fixed_delay_tick_b";

/// Event the timer cell publishes the outcome of a `cancel_timer` command on (`b"ok"`/`b"err"`).
const CANCEL_RESULT_EVENT: &str = "timer_cancel_result";

/// Event receive timeout — generous enough to account for device boot, Zenoh routing,
/// and the 200 ms timer period.
const TICK_TIMEOUT: Duration = Duration::from_secs(30);

/// Smoke test: cell creates a 200 ms periodic timer in `init_cell`.
/// Verifies that at least one tick event arrives on the host side.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn timer_init_smoke() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(TIMER_INIT_CELL)), TIMER_INIT_SRI)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    // Subscribe before loading the cell so we don't miss the first tick.
    let mut ctx = spawned.connect_deferred().await;
    let mut events = ctx.subscribe_cell_event(TICK_EVENT).await;
    ctx.load_cells().await;

    let result = tokio::time::timeout(TICK_TIMEOUT, events.receive())
        .await
        .expect("timed out waiting for first tick event");
    assert_matches!(result, Ok(payload) if payload == b"\"tick\"");
}

/// Cell creates a 200 ms periodic timer in response to a command.
/// Verifies that at least two consecutive tick events arrive, confirming periodicity.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn timer_periodic_via_command() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(TIMER_CMD_CELL)), TIMER_CMD_SRI)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut events = ctx.subscribe_cell_event(TICK_EVENT).await;
    ctx.load_cells().await;

    ctx.command_send(TIMER_CMD_SRI, "start_periodic", None)
        .await;

    let first = tokio::time::timeout(TICK_TIMEOUT, events.receive())
        .await
        .expect("timed out waiting for first tick event");
    assert_ok!(first);

    // Second tick must follow within a small multiple of the 200 ms period — catches
    // broken periodicity (one-shot mis-firing as periodic, wrong period, etc.) that a
    // generous TICK_TIMEOUT would mask.
    let second = tokio::time::timeout(Duration::from_secs(2), events.receive())
        .await
        .expect("second tick did not arrive within 2s of the first");
    assert_ok!(second);
}

/// Cancelling an already-expired one-shot timer must fail, matching the Linux host behaviour
/// (`cancel_timer` returns negative on unknown ID). The cell reports the outcome on the
/// `timer_cancel_result` event.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn cancel_expired_timer_returns_error() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(TIMER_CMD_CELL)), TIMER_CMD_SRI)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut events = ctx.subscribe_cell_event(TICK_EVENT).await;
    ctx.load_cells().await;

    // Create a 500 ms one-shot timer; the host removes it from its table once it fires.
    ctx.command_send(TIMER_CMD_SRI, "start_delayed", None).await;

    // Wait until the tick arrives — by then the timer manager has already removed the entry.
    let tick = tokio::time::timeout(TICK_TIMEOUT, events.receive())
        .await
        .expect("timed out waiting for one-shot tick");
    assert_matches!(tick, Ok(payload) if payload == b"\"tick\"");

    // Cancelling the now-expired handle must fail (unknown timer ID). The cell publishes the
    // outcome on `timer_cancel_result`; `b"err"` signals the failed cancel.
    let result = ctx
        .command_await_event(TIMER_CMD_SRI, "cancel_timer", None, CANCEL_RESULT_EVENT)
        .await;
    assert_eq!(result, b"err", "expected cancel of expired timer to fail");
}

/// Cell creates a periodic timer, then cancels it.
/// Verifies that tick events stop arriving after cancellation.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn timer_cancellation() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell(TIMER_CMD_CELL)), TIMER_CMD_SRI)
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut events = ctx.subscribe_cell_event(TICK_EVENT).await;
    ctx.load_cells().await;

    ctx.command_send(TIMER_CMD_SRI, "start_periodic", None)
        .await;

    // Confirm the timer is running before cancelling.
    let received = tokio::time::timeout(TICK_TIMEOUT, events.receive())
        .await
        .expect("timed out waiting for tick event before cancel");
    assert_ok!(received);

    ctx.command_send(TIMER_CMD_SRI, "cancel_timer", None).await;

    // Verify ticks eventually stop. WAMR processes ticks serially with a Zenoh publish per
    // tick, so cancel propagation plus the in-flight pipeline (CELL_MSG_CHANNEL cap 8 +
    // cell_message_queue + WAMR-in-flight) can leak ~10 buffered ticks after the cancel was
    // sent. Rather than assert immediate silence, drain events until we observe a stable
    // quiet period (3 s without any event), with a hard upper bound to catch a real failure
    // (timer still firing).
    let test_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut last_event = tokio::time::Instant::now();
    let quiet_threshold = Duration::from_secs(3);
    loop {
        if tokio::time::Instant::now() > test_deadline {
            panic!("timer did not stop firing within 20s of cancel");
        }
        let recv_timeout = quiet_threshold
            .saturating_sub(tokio::time::Instant::now().duration_since(last_event))
            .max(Duration::from_millis(50));
        match tokio::time::timeout(recv_timeout, events.receive()).await {
            Ok(Ok(_)) => last_event = tokio::time::Instant::now(),
            Ok(Err(e)) => panic!("failed while draining timer events after cancel: {e}"),
            Err(_) => {
                if tokio::time::Instant::now().duration_since(last_event) >= quiet_threshold {
                    break;
                }
            }
        }
    }
}

/// Two fixed-delay timers run concurrently.
/// Verifies that both emit events, proving neither stalls the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn fixed_delay_two_timers_both_fire() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(
            assert_ok!(build_aot_cell(FIXED_DELAY_CELL)),
            FIXED_DELAY_SRI,
        )
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    let mut ctx = spawned.connect_deferred().await;
    let mut events_a = ctx.subscribe_cell_event(FIXED_DELAY_TICK_A).await;
    let mut events_b = ctx.subscribe_cell_event(FIXED_DELAY_TICK_B).await;
    ctx.load_cells().await;

    // Both timers must fire at least twice within a generous window.
    for _ in 0..2 {
        tokio::time::timeout(TICK_TIMEOUT, events_a.receive())
            .await
            .expect("timed out waiting for fixed_delay_tick — timer A may have stalled")
            .expect("error receiving fixed_delay_tick");
        tokio::time::timeout(TICK_TIMEOUT, events_b.receive())
            .await
            .expect("timed out waiting for fixed_delay_tick_b — timer B may have stalled")
            .expect("error receiving fixed_delay_tick_b");
    }
}
