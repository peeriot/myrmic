//! Native deploy of HTTP/MQTT bridge cells.
//!
//! Bridge cells only need a zenoh session and a mailbox listener, not a WASM execution
//! runtime, so they are spawned directly on the orchestrator: no manifest, no operator
//! runner, no deployment record. Each bridge is tracked by its own [`Sri`] in a
//! process-wide registry (see [`bridge_cells`]) so a later undeploy can find and
//! terminate it again.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use cell_protocol::{PlacementKind, Sri};
use sorg_common::{
    HttpBridgeApi, HttpBridgeRecord, MqttBridge, MqttBridgeDef, MqttBridgeRecord,
    MqttBrokerAddress, MqttConnection, bail, custom_err,
};
use sorg_execution::bridge::http::HttpBridgeHandle;
use sorg_execution::bridge::mqtt::MqttBridgeHandle;
use tokio::sync::oneshot;
use tracing::warn;

use crate::Result;
use crate::error::deleted_element_err;
use crate::event_loop::Runtime;

/// Mirrors the exec plugin's default mailbox poll interval
/// (`sorg_common::configs::exec::Config::mailbox_poll_interval`). Bridge cells run
/// natively on the orchestrator, which has no exec config of its own to read this from.
const MAILBOX_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Signals a running bridge cell's background task to stop: dropping or sending on it
/// resolves the task's `select!`, which drops the bridge handle and, with it, aborts the
/// mailbox listener tasks the handle owns.
type BridgeKillSwitch = oneshot::Sender<()>;

/// Process-wide registry of natively spawned bridge cells, keyed by their own SRI.
///
/// The orchestration `Runtime` is re-created for every event (see
/// `event_loop::set_up_event_loop`), so it cannot hold this state itself; this is the
/// persistent home that survives between the deploy call that spawns a bridge and the
/// later undeploy call that terminates it.
fn bridge_cells() -> &'static Mutex<HashMap<Sri, BridgeKillSwitch>> {
    static BRIDGE_CELLS: OnceLock<Mutex<HashMap<Sri, BridgeKillSwitch>>> = OnceLock::new();
    BRIDGE_CELLS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Spawns `run` as the bridge cell's background task and registers a kill switch for it
/// under `sri`, so a later [`Runtime::terminate_bridge_cell`] call can tear it down. Any
/// previous entry for the same `sri` is dropped, which terminates it too — self-healing
/// against a stale registration.
fn register_bridge_cell<F>(sri: &Sri, run: F) -> PlacementKind
where
    F: Future<Output = sorg_execution::Result<()>> + Send + 'static,
{
    let (poison_snd, poison_rcv) = oneshot::channel();
    tokio::spawn(async move {
        tokio::select! {
            _ = poison_rcv => {}
            result = run => {
                if let Err(err) = result {
                    warn!("bridge cell task ended with an error: {err}");
                }
            }
        }
    });

    bridge_cells()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(*sri, poison_snd);

    PlacementKind::Bridge { sri: *sri }
}

impl Runtime {
    pub(super) async fn deploy_http_bridge(
        &self,
        sri: &Sri,
        api: HttpBridgeApi,
    ) -> Result<PlacementKind> {
        let record = HttpBridgeRecord {
            api: vec![HttpBridgeApi {
                cell_name: sri.to_string(),
                ..api
            }],
        };

        let mut handle = HttpBridgeHandle::new(record, &self.session, MAILBOX_POLL_INTERVAL);
        handle
            .init()
            .await
            .map_err(|err| custom_err!("failed to spawn http bridge '{sri}': {err}"))?;

        Ok(register_bridge_cell(sri, async move { handle.run().await }))
    }

    pub(super) async fn deploy_mqtt_bridge(
        &self,
        sri: &Sri,
        bridge: MqttBridge,
    ) -> Result<PlacementKind> {
        let bridge_def = mqtt_bridge_def(sri, bridge)?;
        let record = MqttBridgeRecord {
            bridges: vec![bridge_def],
        };

        let mut handle = MqttBridgeHandle::new(record, &self.session, MAILBOX_POLL_INTERVAL);
        handle
            .init()
            .await
            .map_err(|err| custom_err!("failed to spawn mqtt bridge '{sri}': {err}"))?;

        Ok(register_bridge_cell(sri, async move { handle.run().await }))
    }

    /// Terminates a natively spawned bridge cell.
    ///
    /// Returns [`crate::Error::AlreadyDeleted`] if no live bridge is registered under
    /// `sri` — it never spawned, or has already been terminated. This is a defined,
    /// clean error rather than a panic.
    pub(crate) fn terminate_bridge_cell(&self, sri: &Sri) -> Result<()> {
        let removed = bridge_cells()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(sri);

        let orch_id = self.session.zid();
        match removed {
            Some(kill_switch) => {
                // The receiving end may already be gone if the task ended on its own
                // (e.g. an internal failure) — either way, the task is no longer running.
                let _ = kill_switch.send(());
                Ok(())
            }
            None => Err(deleted_element_err(format!(
                "bridge cell '{sri}' (checked on orch '{orch_id}')"
            ))),
        }
    }
}

/// Builds the mailbox-facing MQTT bridge definition from the wire config, parsing
/// `broker` (e.g. `"mqtt://host:1883"`) into a connection descriptor (host, port).
fn mqtt_bridge_def(sri: &Sri, bridge: MqttBridge) -> Result<MqttBridgeDef> {
    let MqttBridge {
        cell_name: _,
        broker,
        egress,
        ingress,
    } = bridge;

    let Some((scheme, rest)) = broker.split_once("://") else {
        bail!("mqtt bridge '{sri}': broker '{broker}' has no scheme");
    };

    let default_port = match scheme {
        "mqtt" => 1883,
        "mqtts" => 8883,
        other => bail!(
            "mqtt bridge '{sri}': unsupported broker scheme '{other}', expected 'mqtt' or 'mqtts'"
        ),
    };

    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port_str)) => {
            let port = port_str
                .parse::<u16>()
                .map_err(|err| custom_err!("mqtt bridge '{sri}': invalid broker port: {err}"))?;
            (host, port)
        }
        None => (rest, default_port),
    };

    let connection = MqttConnection {
        broker_address: MqttBrokerAddress::new(host)?,
        broker_port: port,
        keep_alive_period: Duration::from_secs(10),
        client_id: format!("mqtt-bridge-{sri}"),
        channel_cap: 16,
    };

    Ok(MqttBridgeDef {
        cell_name: sri.to_string(),
        connection,
        egress,
        ingress,
    })
}

#[cfg(test)]
mod tests {
    use super::{Sri, bridge_cells, register_bridge_cell};

    // Exercises the registry mechanics `Runtime::terminate_bridge_cell` relies on:
    // removing an sri that was never registered, and removing one a second time after
    // it was already taken, both find nothing — the "never spawned or already
    // terminated" edge case is a clean `None`, not a panic.

    #[tokio::test]
    async fn removing_a_never_registered_sri_finds_nothing() {
        let sri = Sri::from_target("unit-test-bridge-never-registered").unwrap();
        assert!(bridge_cells().lock().unwrap().remove(&sri).is_none());
    }

    #[tokio::test]
    async fn removing_an_already_removed_sri_finds_nothing() {
        let sri = Sri::from_target("unit-test-bridge-already-removed").unwrap();
        let kind = register_bridge_cell(&sri, async { Ok(()) });
        assert!(matches!(kind, super::PlacementKind::Bridge { sri: s } if s == sri));

        assert!(
            bridge_cells().lock().unwrap().remove(&sri).is_some(),
            "the freshly registered bridge should be found once"
        );
        assert!(
            bridge_cells().lock().unwrap().remove(&sri).is_none(),
            "a second removal of the same sri should find nothing, not panic"
        );
    }
}
