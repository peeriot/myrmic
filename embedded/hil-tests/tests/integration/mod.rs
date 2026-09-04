//! Shared test infrastructure for embedded Cell integration tests.
//!
//! Tests require a real ESP32 running modem-esp32 firmware on the same network. Set the
//! `EMBEDDED_TARGET` environment variable to enable them and select the chip; without it every
//! test skips:
//!
//! ```sh
//! EMBEDDED_TARGET=ESP32C6 cargo nextest run -p hil-tests
//! ```
//!
//! `EMBEDDED_TARGET` accepts `ESP32C5`, `ESP32C6`, `ESP32C61`. The firmware that gets
//! flashed defaults to the release build for that chip's ISA and can be overridden with
//! `EMBEDDED_ELF`. The firmware has to be pre-built as this doesn't compile the firmware.

mod aot;
mod cells;
mod espflash;
mod signal_layer;
mod watchdog;

use std::time::Duration;

use test_framework::scenario::{SwarmTest, SwarmTestBuilder};

use crate::integration::aot::aot_target;

/// The device provides the exec runtime, so it has to boot, associate to WiFi (which routinely
/// needs a couple of attempts), take a DHCP lease and find the router before it can register.
/// This timeout is a sensible value for the hardware to do all the aforementioned steps before
/// connecting to swarm.
pub(crate) const DEVICE_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(120);

/// Deploys and synchronous commands travel to the device, which fetches the cell blob over the
/// network before it can answer — far slower than a host round-trip. Matches the orchestrator's
/// `init_timeout_secs` in `tests/data/swarm.jsonnet`.
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(120);

/// Capability tag of the swarm process' own in-process exec runtime, used by the mixed
/// embedded/Linux tests to place the host half of a cell pair.
pub(crate) const TAG_LINUX: &str = "linux";

/// Whether the HIL rig is available. Tests must return early when this is `false` — the whole
/// suite is inert unless `EMBEDDED_TARGET` is set, which both enables the tests and selects the
/// chip under test.
pub(crate) fn device_present() -> bool {
    if std::env::var("EMBEDDED_TARGET").is_err() {
        eprintln!("EMBEDDED_TARGET not set - skipping embedded test");
        return false;
    }
    true
}

/// Whether the rig has the physical sensors of the board manifest wired up. Tests that read a
/// value out of real hardware must return early when this is `false`.
///
/// Separate from [`device_present`] because it describes how the rig is *wired* rather than
/// whether a board is attached: a bare devboard runs the same firmware and passes everything
/// except the tests that need a sensor to answer. That is a supported configuration, not a
/// broken rig, so the skip is deliberate and logged rather than a failure.
pub(crate) fn sensors_present() -> bool {
    if std::env::var("HIL_SENSORS").is_err() {
        eprintln!("HIL_SENSORS not set - skipping test that needs physical sensors");
        return false;
    }
    true
}

/// A [`SwarmTest`] builder pre-wired for the HIL rig: the shared swarm config, the tag of the
/// device under test, and the timeouts real hardware needs. Also installs the test log
/// subscriber, so callers only add their cells.
pub(crate) fn hil_swarm_test() -> SwarmTestBuilder {
    sorg_tests::enable_test_logging("info");

    SwarmTest::builder()
        .config(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/swarm.jsonnet"
        ))
        .tags(&[aot_target()])
        .exec_runtime_timeout(DEVICE_REGISTRATION_TIMEOUT)
        .query_timeout(QUERY_TIMEOUT)
}
