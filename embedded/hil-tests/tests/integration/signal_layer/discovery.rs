//! Tap-registry discovery: every tap the `hil-tests` pipeline declares is
//! present in the running firmware and enumerable from a cell.

use crate::integration::device_present;
use crate::integration::signal_layer::{deploy_bridge, tap_names};

/// Every tap declared in `hil-tests.yaml` (plus the auto `_signal_layer_health`
/// event tap) is registered and enumerable via `tap_list_*`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn tap_registry_lists_expected_taps() {
    if !device_present() {
        return;
    }

    let (mut ctx, _monitor) = deploy_bridge().await;

    let names = tap_names(&mut ctx).await;

    for expected in [
        "_signal_layer_health",
        "temperature",
        "humidity",
        "pressure",
        "avg_temperature",
        "lux",
        "eco2",
        "tvoc",
        "sim_value",
        "sim_alarm",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "expected tap '{expected}' not registered; got {names:?}"
        );
    }
}
