use std::time::{Duration, Instant};

use claims::{assert_none, assert_ok};
use sorg_tests::{build_and_register_cell_class, swarm_config};

use crate::integration::spawn_test_app_with_swarm;

const TIMER_INIT_SRI: &str = "timer_init_cell";
const TIMER_CMD_SRI: &str = "timer_cmd_cell";
const TICK_EVENT: &str = "timer_tick";

/// Smoke test: cell sets up a periodic timer in init, test receives at least one event.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn timer_init_smoke() {
    // Arrange - build the init-based timer cell and deploy it
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-timer-init-logic",
        "timer_init",
        &swarm,
    )
    .await;
    let mut test_app = spawn_test_app_with_swarm(swarm).await;

    // Subscribe before loading the cell
    let mut event_queue = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell("timer_init.wasm".to_owned(), TIMER_INIT_SRI.to_owned())
        .await;

    // Assert - we should receive at least one tick event
    let received = assert_ok!(event_queue.receive().await);
    assert!(!received.is_empty(), "tick event should have a payload");
}

/// Cell creates a periodic timer in response to a command. Test receives multiple events.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn timer_periodic_via_command() {
    // Arrange - build and deploy the command-driven timer cell
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-timer-cmd-logic",
        "timer_cmd",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell("timer_cmd.wasm".to_owned(), TIMER_CMD_SRI.to_owned())
        .await;

    // Act - command the cell to start a periodic timer
    test_app
        .command_send(TIMER_CMD_SRI, "start_periodic", None)
        .await;

    // Assert - receive at least two events to confirm periodicity
    let first = assert_ok!(event_queue.receive().await);
    assert!(!first.is_empty());
    let second = assert_ok!(event_queue.receive().await);
    assert!(!second.is_empty());
}

/// Cell creates a periodic timer. The gap between consecutive events respects the configured period.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn timer_respects_period() {
    // Arrange - build and deploy the command-driven timer cell
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-timer-cmd-logic",
        "timer_cmd",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell("timer_cmd.wasm".to_owned(), TIMER_CMD_SRI.to_owned())
        .await;

    // Act - start a periodic timer (period: 200ms) and measure gaps
    test_app
        .command_send(TIMER_CMD_SRI, "start_periodic", None)
        .await;

    // Discard the first event (timing of first tick may include setup overhead)
    assert_ok!(event_queue.receive().await);

    // Measure gap between second and third events
    let t0 = Instant::now();
    assert_ok!(event_queue.receive().await);
    let gap = t0.elapsed();

    // Assert - gap should be close to 200ms (allow 100ms–400ms tolerance)
    assert!(
        gap > Duration::from_millis(100) && gap < Duration::from_millis(400),
        "expected gap ~200ms, got {gap:?}"
    );
}

/// Cell creates a delayed one-shot. Event is not emitted immediately, arrives after delay.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn timer_delayed_one_shot() {
    // Arrange - build and deploy the command-driven timer cell
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-timer-cmd-logic",
        "timer_cmd",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell("timer_cmd.wasm".to_owned(), TIMER_CMD_SRI.to_owned())
        .await;

    // Act - command the cell to start a delayed one-shot (delay: 500ms)
    test_app
        .command_send(TIMER_CMD_SRI, "start_delayed", None)
        .await;

    // Assert I - no event should be available shortly after triggering (delay is 500ms)
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_none!(assert_ok!(event_queue.try_receive().await));

    // Assert II - event should arrive after the delay
    let received = assert_ok!(event_queue.receive().await);
    assert!(!received.is_empty(), "delayed event should have a payload");
}

/// Cell creates a periodic timer with count=3. Exactly 3 events are received.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn timer_finite_count() {
    // Arrange - build and deploy the command-driven timer cell
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-timer-cmd-logic",
        "timer_cmd",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell("timer_cmd.wasm".to_owned(), TIMER_CMD_SRI.to_owned())
        .await;

    // Act - command the cell to start a timer with count=3, period=200ms
    test_app
        .command_send(TIMER_CMD_SRI, "start_counted", None)
        .await;

    // Assert I - receive exactly 3 events
    for i in 0..3 {
        let received = assert_ok!(event_queue.receive().await);
        assert!(!received.is_empty(), "tick {i} should have a payload");
    }

    // Assert II - no more events after that
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_none!(assert_ok!(event_queue.try_receive().await));
}

/// Cell creates a periodic timer, then cancels it via a second command. Events stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn timer_cancellation() {
    // Arrange - build and deploy the command-driven timer cell
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-timer-cmd-logic",
        "timer_cmd",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell("timer_cmd.wasm".to_owned(), TIMER_CMD_SRI.to_owned())
        .await;

    // Act I - start a periodic timer and confirm it's ticking
    test_app
        .command_send(TIMER_CMD_SRI, "start_periodic", None)
        .await;

    // Act II - cancel the timer
    test_app
        .command_send(TIMER_CMD_SRI, "cancel_timer", None)
        .await;

    // The test is set to start a 200ms periodic timer.
    // So, 16 events is 200ms * 32 = 6_400ms, so as long as this test doesn't run for longer than that, we're fine,
    // and we just drained the event queue.
    let received = assert_ok!(event_queue.receive_batch(32).await);
    assert!(!received.is_empty());

    // Assert - no more events after cancellation
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_none!(assert_ok!(event_queue.try_receive().await));
}

/// Cell creates a periodic timer with an initial delay via `interval_at`.
/// No event during the delay, then periodic events after.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
pub async fn timer_delayed_periodic() {
    // Arrange
    let swarm = swarm_config!("cells/macros/swarm.jsonnet");
    build_and_register_cell_class(
        "../../tests/fixtures/cell-timer-cmd-logic",
        "timer_cmd",
        &swarm,
    )
    .await;

    let mut test_app = spawn_test_app_with_swarm(swarm).await;
    let mut event_queue = test_app.subscribe_cell_event(TICK_EVENT).await;

    test_app
        .deploy_wasm_cell("timer_cmd.wasm".to_owned(), TIMER_CMD_SRI.to_owned())
        .await;

    // Act - start a delayed periodic timer (delay: 300ms, period: 200ms)
    test_app
        .command_send(TIMER_CMD_SRI, "start_delayed_periodic", None)
        .await;

    // Assert I - Wait until the first event
    // drain it as well.
    let events = assert_ok!(event_queue.receive_batch(16).await);
    assert!(!events.is_empty());

    // Assert II - a second event arrives (proving it's periodic, not one-shot)
    let events = assert_ok!(event_queue.receive_batch(16).await);
    assert!(!events.is_empty());
}

// PARKED(new-model): in-cell errors are invisible under fire-and-forget; needs an error-via-event redesign. Revisit.
// /// Cell hits the per-cell timer limit. The 6th creation fails. After cancelling
// /// one, creation succeeds again.
// #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
// pub async fn timer_limit_exceeded() {
//     // Arrange
//     let swarm = swarm_config!("cells/macros/swarm.jsonnet");
//     build_and_register_cell_class(
//         "../../tests/fixtures/cell-timer-cmd-logic",
//         "timer_cmd",
//         &swarm,
//     )
//     .await;
//
//     let test_app = spawn_test_app_with_swarm(swarm).await;
//
//     test_app
//         .deploy_wasm_cell("timer_cmd.wasm".to_owned(), TIMER_CMD_SRI.to_owned())
//         .await;
//
//     let sri = Sri::new(TIMER_CMD_SRI);
//
//     // Act I - create 5 timers (the limit), all should succeed
//     for _ in 0..5 {
//         assert_ok!(
//             test_app
//                 .command_send_wait(sri.clone(), "start_periodic")
//                 .await
//         );
//     }
//
//     // Assert I - the 6th should fail
//     let result = test_app
//         .command_send_wait(sri.clone(), "start_periodic")
//         .await;
//     assert_err!(result);
//
//     // Act II - cancel one timer to free a slot
//     assert_ok!(
//         test_app
//             .command_send_wait(sri.clone(), "cancel_timer")
//             .await
//     );
//
//     // Assert II - creating a new timer should succeed again
//     assert_ok!(
//         test_app
//             .command_send_wait(sri.clone(), "start_periodic")
//             .await
//     );
// }

// PARKED(new-model): in-cell errors are invisible under fire-and-forget; needs an error-via-event redesign. Revisit.
// /// Cell tries to create a timer with a non-existent export name. The command errors.
// #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
// pub async fn timer_invalid_export_errors() {
//     // Arrange - build and deploy the command-driven timer cell
//     let swarm = swarm_config!("cells/macros/swarm.jsonnet");
//     build_and_register_cell_class(
//         "../../tests/fixtures/cell-timer-cmd-logic",
//         "timer_cmd",
//         &swarm,
//     )
//     .await;
//
//     let test_app = spawn_test_app_with_swarm(swarm).await;
//
//     test_app
//         .deploy_wasm_cell("timer_cmd.wasm".to_owned(), TIMER_CMD_SRI.to_owned())
//         .await;
//
//     // Act - command the cell to create a timer with an invalid export name
//     let result = test_app
//         .command_send_wait(Sri::new(TIMER_CMD_SRI), "start_invalid")
//         .await;
//
//     // Assert - the command should fail
//     assert_err!(result);
// }
