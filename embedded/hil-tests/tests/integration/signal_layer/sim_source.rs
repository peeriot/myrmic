//! Deterministic dataplane tests driven by the synthetic `sim` source, which
//! ramps `0, 10, 20, …, 100, 0, …` every 500 ms independent of any hardware.

use std::time::Duration;

use signal_layer_types::ThresholdAlarm;

use crate::integration::device_present;
use crate::integration::signal_layer::{
    EVENT_ALARM, deploy_bridge, drain_events, read_tap_f32, spawn_bridge_deferred, wait_for_tap,
};

/// The `sim_value` retained tap holds a value on the known ramp, and it advances
/// between samples (the source is live).
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sim_value_ramps_on_the_expected_grid() {
    if !device_present() {
        return;
    }

    let (mut ctx, _monitor) = deploy_bridge().await;

    let (ts1, value) = wait_for_tap(&mut ctx, "sim_value", Duration::from_secs(10)).await;

    // The ramp only ever lands on multiples of 10 within [0, 100].
    assert!(
        (0.0..=100.0).contains(&value),
        "sim_value {value} outside ramp bounds [0, 100]"
    );
    assert!(
        (value % 10.0).abs() < 1e-3,
        "sim_value {value} is not a multiple of 10"
    );

    // A later read must carry a newer timestamp — the source keeps sampling.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let (ts2, _) = read_tap_f32(&mut ctx, "sim_value")
        .await
        .expect("sim_value disappeared");
    assert!(
        ts2 > ts1,
        "sim_value timestamp did not advance ({ts1} -> {ts2})"
    );
}

/// The `threshold-trigger` step over the synthetic ramp emits a `ThresholdAlarm`
/// event on the rising edge just above its threshold (value 60 for threshold 50,
/// since the trigger is strict `>`), republished onto `tap_alarm`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sim_threshold_trigger_fires_at_the_threshold() {
    if !device_present() {
        return;
    }

    // Subscribe before draining so we can't miss the republished alarm.
    let (mut ctx, _monitor) = spawn_bridge_deferred().await;
    let mut alarms = ctx.subscribe_cell_event(EVENT_ALARM).await;
    ctx.load_cells().await;

    // Alarms fire once per ramp cycle (~5.5 s) and are retained in the event tap
    // until drained. Drive the bridge to drain + republish, then read one off.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let bytes = loop {
        drain_events(&ctx).await;
        if let Ok(Some(payload)) = alarms.try_receive().await {
            break payload;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no threshold alarm republished within timeout"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    let alarm: ThresholdAlarm =
        postcard::from_bytes(&bytes).expect("failed to decode ThresholdAlarm");

    // The threshold-trigger fires on the rising edge into `value > threshold`
    // (strict — equal does not fire). The ramp steps 0,10,…,50,60,…, so the
    // first sample strictly above 50 is 60. It re-fires at 60 every cycle after
    // the ramp wraps, so the alarm value is deterministically 60.
    assert!(
        (alarm.threshold - 50.0).abs() < 1e-3,
        "unexpected alarm threshold {}",
        alarm.threshold
    );
    assert!(
        (alarm.value - 60.0).abs() < 1e-3,
        "unexpected alarm value {} (expected 60, the first ramp step above 50)",
        alarm.value
    );
}
