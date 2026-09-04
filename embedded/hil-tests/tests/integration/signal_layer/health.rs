//! Driver-health test: the intentionally-absent CCS811 (`air_quality`) source
//! fails bring-up and emits a `Down` health event, and its retained taps stay
//! empty (never written) rather than reporting a value.
//!
//! This needs no CCS811 on the rig — its absence *is* the test. It exercises the
//! generated per-source health state machine and the tap read path for a source
//! that never produces data.

use std::time::Duration;

use signal_layer_types::{DriverHealth, HealthEvent};

use crate::integration::device_present;
use crate::integration::signal_layer::{
    CCS811_SOURCE_IDX, EVENT_HEALTH, deploy_bridge, drain_events, read_tap_f32,
    spawn_bridge_deferred,
};

/// The absent CCS811 source emits a `Down` health event for its source index.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn absent_sensor_emits_down_health_event() {
    if !device_present() {
        return;
    }

    // Subscribe before draining so the republished event can't be missed.
    let (mut ctx, _monitor) = spawn_bridge_deferred().await;
    let mut health = ctx.subscribe_cell_event(EVENT_HEALTH).await;
    ctx.load_cells().await;

    // Drive the bridge to drain + republish the retained health event(s), then
    // scan for the CCS811 `Down`. Other sources never emit while healthy, but we
    // tolerate unrelated events (e.g. an unwired real sensor) by scanning on.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        drain_events(&ctx).await;

        while let Ok(Some(bytes)) = health.try_receive().await {
            let event: HealthEvent =
                postcard::from_bytes(&bytes).expect("failed to decode HealthEvent");
            if event.source == CCS811_SOURCE_IDX {
                assert_eq!(
                    event.state,
                    DriverHealth::Down,
                    "CCS811 source reported {:?}, expected Down",
                    event.state
                );
                return;
            }
        }

        assert!(
            tokio::time::Instant::now() < deadline,
            "no Down health event for CCS811 (source {CCS811_SOURCE_IDX}) within timeout"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// The absent CCS811's retained taps never hold a value (read empty).
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn absent_sensor_taps_read_empty() {
    if !device_present() {
        return;
    }

    let (mut ctx, _monitor) = deploy_bridge().await;

    // Give the source several bring-up attempts; a populated sensor would have
    // filled these within a second or two.
    tokio::time::sleep(Duration::from_secs(5)).await;

    for tap in ["eco2", "tvoc"] {
        let value = read_tap_f32(&mut ctx, tap).await;
        assert!(
            value.is_none(),
            "tap '{tap}' held {value:?} but its source (CCS811) is absent"
        );
    }
}
