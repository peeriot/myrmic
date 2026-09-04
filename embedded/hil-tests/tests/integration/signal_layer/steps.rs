//! Processing-step test: the `moving-average` step over the real BME280
//! temperature produces the derived `avg_temperature` tap once its window fills.
//!
//! Requires a BME280 physically wired on the rig. The value-correctness of the
//! step algorithm itself is covered by host unit tests in the `moving-average`
//! crate; here we assert the step runs end-to-end on-device and its output is
//! consistent with the source it averages.

use std::time::Duration;

use crate::integration::signal_layer::{deploy_bridge, read_tap_f32, wait_for_tap};
use crate::integration::{device_present, sensors_present};

/// After the 8-sample window fills (≈8 s at a 1 s sample interval), the derived
/// `avg_temperature` tap is populated and tracks the `temperature` tap within a
/// sane band.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn moving_average_produces_avg_temperature() {
    if !device_present() || !sensors_present() {
        return;
    }

    let (mut ctx, _monitor) = deploy_bridge().await;

    // Window is 8 samples at 1 s each; allow boot + fill + slack.
    let (_, avg) = wait_for_tap(&mut ctx, "avg_temperature", Duration::from_secs(30)).await;

    assert!(
        (-40.0..=85.0).contains(&avg),
        "avg_temperature {avg} outside the BME280 operating range"
    );

    // The average of recent temperatures should sit close to the latest reading.
    let (_, temp) = read_tap_f32(&mut ctx, "temperature")
        .await
        .expect("temperature tap empty while avg_temperature is present");
    assert!(
        (avg - temp).abs() < 5.0,
        "avg_temperature {avg} diverges from temperature {temp} by more than 5°C"
    );
}
