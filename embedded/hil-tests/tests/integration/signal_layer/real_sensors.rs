//! Real-sensor tests: the BME280 and VEML7700 drivers, running on-device,
//! decode plausible physical values and keep sampling (their taps stay live).
//!
//! Requires both sensors physically wired on the rig's I2C0 bus. Assertions are
//! range/liveness based — exact values depend on the ambient environment.

use std::time::Duration;

use crate::integration::signal_layer::{deploy_bridge, read_tap_f32, wait_for_tap};
use crate::integration::{device_present, sensors_present};

const TAP_TIMEOUT: Duration = Duration::from_secs(20);

/// BME280 temperature / humidity / pressure taps hold physically plausible
/// values.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bme280_reports_plausible_values() {
    if !device_present() || !sensors_present() {
        return;
    }

    let (mut ctx, _monitor) = deploy_bridge().await;

    let (_, temperature) = wait_for_tap(&mut ctx, "temperature", TAP_TIMEOUT).await;
    assert!(
        (-40.0..=85.0).contains(&temperature),
        "temperature {temperature} °C outside the BME280 operating range"
    );

    let (_, humidity) = wait_for_tap(&mut ctx, "humidity", TAP_TIMEOUT).await;
    assert!(
        (0.0..=100.0).contains(&humidity),
        "humidity {humidity} %RH outside [0, 100]"
    );

    let (_, pressure) = wait_for_tap(&mut ctx, "pressure", TAP_TIMEOUT).await;
    assert!(
        (300.0..=1100.0).contains(&pressure),
        "pressure {pressure} hPa outside plausible atmospheric range"
    );
}

/// The VEML7700 `lux` tap holds a non-negative value.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn veml7700_reports_non_negative_lux() {
    if !device_present() || !sensors_present() {
        return;
    }

    let (mut ctx, _monitor) = deploy_bridge().await;

    let (_, lux) = wait_for_tap(&mut ctx, "lux", TAP_TIMEOUT).await;
    assert!(lux >= 0.0, "lux {lux} is negative");
}

/// A real source keeps sampling: the `temperature` tap's timestamp advances
/// across a sample interval.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bme280_tap_stays_live() {
    if !device_present() || !sensors_present() {
        return;
    }

    let (mut ctx, _monitor) = deploy_bridge().await;

    let (ts1, _) = wait_for_tap(&mut ctx, "temperature", TAP_TIMEOUT).await;

    // BME280 samples every 1 s; wait comfortably past one interval.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let (ts2, _) = read_tap_f32(&mut ctx, "temperature")
        .await
        .expect("temperature tap went empty");
    assert!(
        ts2 > ts1,
        "temperature timestamp did not advance ({ts1} -> {ts2}) — source stalled"
    );
}
