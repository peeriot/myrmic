//! Blinky HIL test using fixed-delay periodic scheduling.
//!
//! Deploys the `cell_blinky_periodic` cell to a real ESP32 device and verifies
//! that `blink` events arrive at a ~500 ms cadence.

use std::time::Duration;

use claims::assert_ok;

use crate::integration::{
    aot::build_aot_cell, device_present, espflash::flash_device, hil_swarm_test,
};

const BLINKY_SRI: &str = "emb_blinky_periodic";
const BLINK_EVENT: &str = "blink";

/// Allow enough time for device boot + Zenoh routing + first 500 ms tick.
const BLINK_TIMEOUT: Duration = Duration::from_secs(30);
/// Fixed-delay starts the next period after the full handler returns, including
/// event publishing and state persistence on the embedded DB path.
const NEXT_BLINK_TIMEOUT: Duration = Duration::from_secs(10);

/// Smoke test: blink events alternate between `on` and `off` payloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn blinky_periodic_alternates() {
    if !device_present() {
        return;
    }

    let spawned = hil_swarm_test()
        .aot_cell(
            assert_ok!(build_aot_cell("cell-blinky-periodic-logic")),
            BLINKY_SRI,
        )
        .spawn()
        .await;

    let _monitor = assert_ok!(flash_device());

    // Subscribe before loading the cell so the first event is not missed.
    let mut ctx = spawned.connect_deferred().await;
    let mut events = ctx.subscribe_cell_event(BLINK_EVENT).await;
    ctx.load_cells().await;

    // First tick: LED turns on.
    let first = tokio::time::timeout(BLINK_TIMEOUT, events.receive())
        .await
        .expect("timed out waiting for first blink event");
    assert_ok!(&first);
    assert_eq!(first.unwrap(), b"on", "expected first blink to be 'on'");

    let second = tokio::time::timeout(NEXT_BLINK_TIMEOUT, events.receive())
        .await
        .expect("timed out waiting for second blink event");
    assert_ok!(&second);
    assert_eq!(second.unwrap(), b"off", "expected second blink to be 'off'");
}
