//! Signal-layer (dataplane) HIL tests.
//!
//! These run against the **`pipeline`-enabled** `modem-esp32` firmware (a
//! separate ELF from the plain cell-test firmware), built from the `hil-tests`
//! pipeline on the per-chip HIL board manifest (`esp32c6-hil` here; use
//! `esp32c5-hil` / `esp32c61-hil` for those chips):
//!
//! ```sh
//! cd sdk/signal-layer && scripts/build.sh hil-tests --board esp32c6-hil
//! EMBEDDED_TARGET=ESP32C6 EMBEDDED_ELF=<path-to-pipeline-elf> \
//!   cargo nextest run -p hil-tests signal_layer
//! ```
//!
//! (If the pipeline firmware is left at the default target path, `EMBEDDED_ELF`
//! can be omitted.) Like the rest of the suite they are inert unless
//! `EMBEDDED_TARGET` is set.
//!
//! The rig: BME280 + VEML7700 wired on I2C0 (real-value tests), CCS811 declared
//! but **not populated** (health `Down` test), and a synthetic `sim` source
//! (deterministic value / step / alarm tests). Taps are read over Zenoh through
//! the `tap-bridge` cell.

mod discovery;
mod health;
mod real_sensors;
mod sim_source;
mod steps;

use std::time::Duration;

use claims::assert_ok;
use test_framework::clients::sorg::EventQueue;
use test_framework::scenario::SwarmTestCtx;
use tokio::time::{Instant, sleep, timeout};

use crate::integration::{
    aot::build_aot_cell,
    espflash::{MonitorHandle, flash_device},
    hil_swarm_test,
};

/// SRI the `tap-bridge` cell is deployed under across these tests.
const BRIDGE_SRI: &str = "tap_bridge";

/// Zenoh event the bridge republishes drained `_signal_layer_health` events onto.
const EVENT_HEALTH: &str = "tap_health";
/// Zenoh event the bridge republishes drained `sim_alarm` events onto.
const EVENT_ALARM: &str = "tap_alarm";

/// Source index of the CCS811 (`air_quality`) source in the pipeline — the
/// intentionally-absent sensor whose `Down` health event the health test
/// asserts. Matches the source order in `hil-tests.yaml`.
const CCS811_SOURCE_IDX: u8 = 2;

/// Bring up the rig with the `tap-bridge` cell deployed on the device.
///
/// Use in tests that only read retained taps; event tests must use
/// [`spawn_bridge_deferred`] so they can subscribe before the bridge republishes anything.
async fn deploy_bridge() -> (SwarmTestCtx, MonitorHandle) {
    let (mut ctx, monitor) = spawn_bridge_deferred().await;
    ctx.load_cells().await;
    (ctx, monitor)
}

/// [`deploy_bridge`] stopping short of loading the bridge cell, so the caller can subscribe to
/// the republished events first. Finish with [`SwarmTestCtx::load_cells`].
async fn spawn_bridge_deferred() -> (SwarmTestCtx, MonitorHandle) {
    let spawned = hil_swarm_test()
        .aot_cell(assert_ok!(build_aot_cell("tap-bridge")), BRIDGE_SRI)
        .spawn()
        .await;

    let monitor = assert_ok!(flash_device());

    let ctx = spawned.connect_deferred().await;
    (ctx, monitor)
}

/// Poll interval for [`wait_for_tap`].
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long to wait for the `tap-bridge` cell to publish the event reply to a command.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll a retained `f32` tap until it holds a value, returning `(ts_ms, value)`.
/// Panics if the tap never produces a value within `timeout`.
async fn wait_for_tap(ctx: &mut SwarmTestCtx, tap: &str, timeout: Duration) -> (u64, f32) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = read_tap_f32(ctx, tap).await {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "tap '{tap}' produced no value within {timeout:?}"
        );
        sleep(POLL_INTERVAL).await;
    }
}

/// Reads a retained `f32` signal-layer tap through the deployed `tap-bridge` cell.
///
/// Commands are fire-and-forget, so the bridge answers `read_tap` by publishing its result on the
/// `tap_value` event. This subscribes to that event first, sends the command with the tap name,
/// then decodes the status-prefixed reply: `[1][ts_ms: u64 LE][value: f32 LE]` (13 bytes) when the
/// tap holds a value, `[0]` when it is unknown or currently empty (e.g. cleared after a driver
/// fault). Returns `Some((ts_ms, value))` or `None` accordingly. Panics if the bridge never
/// replies within [`REPLY_TIMEOUT`] or the reply is malformed - both are transport/protocol
/// anomalies rather than "no value".
async fn read_tap_f32(ctx: &mut SwarmTestCtx, tap: &str) -> Option<(u64, f32)> {
    // Subscribe before commanding so the reply can't be missed.
    let mut replies = ctx.subscribe_cell_event("tap_value").await;
    ctx.command_send(BRIDGE_SRI, "read_tap", Some(tap.as_bytes().to_vec()))
        .await;

    let bytes = recv_reply(&mut replies, "read_tap").await;

    match bytes.as_slice() {
        [0] => None,
        [1, rest @ ..] if rest.len() == 12 => Some((
            u64::from_le_bytes(rest[..8].try_into().unwrap()),
            f32::from_le_bytes(rest[8..12].try_into().unwrap()),
        )),
        other => panic!("read_tap('{tap}') unexpected reply {other:?}"),
    }
}

/// Drives the `tap-bridge` cell to drain its event taps (`_signal_layer_health`, `sim_alarm`) and
/// republish each as a Zenoh event (`tap_health`, `tap_alarm`). Fire-and-forget: the caller
/// observes the republished events on its own subscriptions.
async fn drain_events(ctx: &SwarmTestCtx) {
    ctx.command_send(BRIDGE_SRI, "drain_events", None).await;
}

/// Returns the names of every registered signal-layer tap, via the `tap-bridge` cell's `tap_names`
/// command, whose reply the bridge publishes on the `tap_names` event. Panics if the bridge never
/// replies within [`REPLY_TIMEOUT`].
async fn tap_names(ctx: &mut SwarmTestCtx) -> Vec<String> {
    let mut replies = ctx.subscribe_cell_event("tap_names").await;
    ctx.command_send(BRIDGE_SRI, "tap_names", None).await;

    let bytes = recv_reply(&mut replies, "tap_names").await;

    String::from_utf8(bytes)
        .expect("tap_names reply not utf-8")
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Receives one event payload from `queue` within [`REPLY_TIMEOUT`], panicking on timeout or a
/// transport error. `cmd` names the command whose reply we await, for the panic message.
async fn recv_reply(queue: &mut EventQueue, cmd: &str) -> Vec<u8> {
    let Ok(reply) = timeout(REPLY_TIMEOUT, queue.receive()).await else {
        panic!("{cmd} reply not received within {REPLY_TIMEOUT:?}");
    };

    reply.unwrap_or_else(|e| panic!("{cmd} reply transport error: {e:?}"))
}
