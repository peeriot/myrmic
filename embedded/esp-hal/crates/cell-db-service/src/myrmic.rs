//! Myrmic-specific network handling

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;

use cell_protocol::node_tags::{self, NODE_TAGS_TABLE, NodeTagOverlay, node_tags_scope};
use cell_protocol::{
    CapabilityTag, EXEC_REGISTRY_TABLE, ExecRuntimeInfo, ExecutionCapabilities, NODE_LEASE_TABLE,
    NodeLease, RuntimeId, WATCHDOG_RESETS_TABLE, WatchdogResetReport, node_lease_scope,
    replication::runtime_tag, scope_of_exec_registry, scope_of_watchdog_resets,
};
use cfg_match::cfg_match;
use db_client::v1::Client;
use db_client::v1::models::{tb_delete, tb_get, tb_insert};
use embassy_time::{Duration as EmbDuration, with_timeout};
use esp_hal::efuse;
use myrmic_tags::Platform;
use wasm_storage::__reexports::postcard;
use zenoh_nano::scout::ZenohIdProto;
use zenoh_result::{ZResult, zerror};

use crate::service::DEFAULT_TIMEOUT;

/// The period chosen for re-registering the exec runtime in the registry
pub(crate) const REGISTRATION_PERIOD: EmbDuration = EmbDuration::from_secs(5 * 60);
/// The retention period of the exec registration which overlaps the [`REGISTRATION_PERIOD`] to
/// guarantee the entry always lives in the registry (when no problem occurs).
const RETENTION_PERIOD_S: u64 = REGISTRATION_PERIOD.as_secs() + 60;

/// Liveness-lease renewal period, slower than the Linux exec's 10s to
/// respect the radio budget; [`LEASE_TTL_MS`] absorbs the sparser cadence.
pub(crate) const LEASE_RENEW_PERIOD: EmbDuration = EmbDuration::from_secs(30);
/// Retry delay when a renewal could not be written (no swarm time yet, or
/// the db was unreachable).
pub(crate) const LEASE_RETRY_PERIOD: EmbDuration = EmbDuration::from_secs(5);
/// The silence this node asks observers to tolerate: three renewal periods,
/// so a couple of dropped radio rounds never declare it dead.
const LEASE_TTL_MS: u64 = 90_000;
/// Db retention for lease rows: 5 × (ttl + the cluster margin), matching the
/// Linux exec's `lease_retention`, so hygiene always acts before a dead
/// node's last renewal purges.
const LEASE_RETENTION_S: u64 = 5 * (LEASE_TTL_MS / 1000 + 15);

/// The tags this device carries by virtue of what it is: its target, its
/// radios, its peripherals and its own runtime tag. All facts about the
/// hardware, so a tag overlay may add to them but never take one away.
fn intrinsic_tags(zid: ZenohIdProto) -> Vec<String> {
    let target = cfg_match! {
        feature = "esp32c5" => Platform::Esp32c5,
        feature = "esp32c6" => Platform::Esp32c6,
        feature = "esp32c61" => Platform::Esp32c61,
        _ => unreachable!("one target needs to be selected"),
    };
    let mut tags: Vec<String> = target.get_tags().into_iter().map(String::from).collect();

    // Add myrmic transports
    tags.push(String::from("wifi-myrmic"));
    // Add user transports
    if cfg!(feature = "ble") {
        tags.push(String::from(myrmic_tags::TAG_BLE));
    }
    // Add hardware capabilities
    tags.push(String::from("gpio"));
    if cfg!(any(feature = "esp32c5", feature = "esp32c61")) {
        tags.push(String::from("psram"));
    }
    // Add the system tag naming this runtime, so a deploy can pin to it
    tags.push(runtime_tag(zid.into()));

    tags
}

/// Creates the Runtime info for this device, including any tags added to it
/// since it booted.
///
/// The overlay is re-read on every registration round rather than watched: a
/// device that only wakes to register has no cheaper moment to notice a retag,
/// and a read that fails simply leaves the added tags off until the next round.
///
/// The first rounds of a boot drop the overlay instead of reading it, so the
/// tags the device was flashed with have the say after a restart and only a
/// retag that follows this boot takes effect. `overlay_cleared` carries that
/// across rounds: until the row is known to be gone the attempt is repeated,
/// because an overlay left standing by an unreachable db must not come into
/// force later.
pub(crate) async fn create_runtime_info(
    db_client: &Client,
    zid: ZenohIdProto,
    overlay_cleared: &mut bool,
) -> ExecRuntimeInfo {
    let name = option_env!("RUNTIME_NAME").unwrap_or_else(|| {
        cfg_match! {
            feature = "esp32c5" => "ESP32-C5",
            feature = "esp32c6" => "ESP32-C6",
            feature = "esp32c61" => "ESP32-C61",
            _ => unreachable!("one target needs to be selected"),
        }
    });

    let overlay = if *overlay_cleared {
        read_tag_overlay(db_client, zid.into())
            .await
            .unwrap_or_default()
    } else {
        *overlay_cleared = clear_tag_overlay(db_client, zid.into()).await;
        None
    };

    let capabilities: Vec<CapabilityTag> =
        node_tags::effective(overlay.as_ref(), &[], &intrinsic_tags(zid))
            .into_iter()
            .map(CapabilityTag::new)
            .collect();

    ExecRuntimeInfo::new(
        zid,
        Some(String::from(name)),
        ExecutionCapabilities::new(capabilities),
    )
}

/// Deletes this device's tag overlay, reporting whether the row is gone. A
/// device that was never tagged needs no write — a tombstone would only
/// replicate noise.
async fn clear_tag_overlay(db_client: &Client, node: RuntimeId) -> bool {
    match read_tag_overlay(db_client, node).await {
        Ok(None) => true,
        Ok(Some(_)) => {
            let eid = node.to_string().into_bytes();
            let delete = db_client.write_tx_in(node_tags_scope(), async move |client, id| {
                client
                    .send(tb_delete::Request {
                        id,
                        op: tb_delete::Op {
                            scope: node_tags_scope(),
                            table: NODE_TAGS_TABLE.to_owned(),
                            eid,
                        },
                    })
                    .await?
                    .map_err(|err| zerror!("{}", err.message))?;
                Ok(())
            });

            match with_timeout(DEFAULT_TIMEOUT, delete).await {
                Ok(Ok(())) => {
                    log::info!("[tags] dropped this node's tag overlay");
                    true
                }
                _ => {
                    log::warn!("[tags] unable to drop this node's tag overlay");
                    false
                }
            }
        }
        Err(_) => false,
    }
}

/// This device's tag overlay, `None` when it has none, or an error when the db
/// could not be reached — a caller deciding whether the overlay is gone must
/// not read silence as an empty table.
async fn read_tag_overlay(db_client: &Client, node: RuntimeId) -> ZResult<Option<NodeTagOverlay>> {
    let eid = node.to_string().into_bytes();
    let read = db_client.read_tx_in(node_tags_scope(), async move |client, id| {
        let response = client
            .send(tb_get::Request {
                id,
                op: tb_get::Op {
                    scope: node_tags_scope(),
                    table: NODE_TAGS_TABLE.to_owned(),
                    eid,
                },
            })
            .await?
            .map_err(|err| zerror!("{}", err.message))?;
        Ok(response)
    });

    match with_timeout(DEFAULT_TIMEOUT, read).await {
        Ok(Ok(response)) => Ok(response
            .value
            .and_then(|value| postcard::from_bytes(&value).ok())),
        Ok(Err(err)) => {
            log::warn!("[tags] unable to read this node's tag overlay: {err}");
            Err(err)
        }
        Err(_) => {
            log::warn!("[tags] timed out reading this node's tag overlay");
            Err(zerror!("timed out reading the node tags table").into())
        }
    }
}

/// Registers the device as an exec runtime by inserting the relevant information in the DB
pub(crate) async fn register_exec_runtime(db_client: &Client, info: ExecRuntimeInfo) {
    log::info!("Registering exec runtime with info: {info:?}");
    if let Err(e) = db_client
        .write_tx_in_with_retention(
            scope_of_exec_registry(),
            Some(Duration::from_secs(RETENTION_PERIOD_S)),
            async move |client, tx_id| {
                client
                    .send(tb_insert::Request {
                        id: tx_id,
                        op: tb_insert::Op {
                            scope: scope_of_exec_registry(),
                            table: EXEC_REGISTRY_TABLE.to_owned(),
                            eid: Some(info.id().to_string().into_bytes()),
                            value: postcard::to_allocvec(&info).map_err(|e| {
                                zerror!("failed to serialize exec runtime info: {e}")
                            })?,
                        },
                    })
                    .await
                    .map_err(|e| zerror!("unable to register exec: {e}"))?
                    .map_err(|e| zerror!("unable to send insert request: {}", e.message))?;

                Ok(())
            },
        )
        .await
    {
        log::error!("Failed to register runtime in DB: {e}");
    }
}

/// The stable identity of this device: the base MAC address burned into eFuse
/// during manufacturing, as colon-separated hex.
pub(crate) fn device_id() -> String {
    efuse::base_mac_address().to_string()
}

/// Renews this node's liveness lease, mirroring the Linux exec's renewal
/// (`sorg-execution`). `seq` is wall-clock millis: the node id is MAC-stable
/// across reboots, so the seq must keep advancing through one. Before the
/// first clock sync there is no wall time and the renewal is skipped — a
/// near-zero seq would read as ancient silence. Returns whether the lease
/// was written.
pub(crate) async fn renew_node_lease(
    db_client: &Client,
    zid: ZenohIdProto,
    wall_time: fn() -> Option<core::time::Duration>,
) -> bool {
    let Some(now) = wall_time() else {
        log::debug!("[lease] no swarm time yet; renewal deferred");
        return false;
    };
    let lease = NodeLease {
        device_id: device_id(),
        seq: u64::try_from(now.as_millis()).unwrap_or(u64::MAX),
        ttl_ms: LEASE_TTL_MS,
    };
    let id: RuntimeId = zid.into();
    let result = db_client
        .write_tx_in_with_retention(
            node_lease_scope(),
            Some(Duration::from_secs(LEASE_RETENTION_S)),
            async move |client, tx_id| {
                client
                    .send(tb_insert::Request {
                        id: tx_id,
                        op: tb_insert::Op {
                            scope: node_lease_scope(),
                            table: NODE_LEASE_TABLE.to_owned(),
                            eid: Some(id.to_string().into_bytes()),
                            value: postcard::to_allocvec(&lease)
                                .map_err(|e| zerror!("failed to serialize node lease: {e}"))?,
                        },
                    })
                    .await
                    .map_err(|e| zerror!("unable to renew lease: {e}"))?
                    .map_err(|e| zerror!("unable to send insert request: {}", e.message))?;

                Ok(())
            },
        )
        .await;
    if let Err(e) = &result {
        log::warn!("[lease] renewal failed: {e}");
    }
    result.is_ok()
}

/// Reports a hardware-watchdog reset to the swarm (SDS-FEAT-2026-HWD-001
/// Area D) by upserting this device's reset report in the watchdog-resets
/// table. Keyed on [`device_id`], so repeated resets of one device update its
/// single row and the table stays bounded by the number of devices. No
/// retention: the record documents reset history, it must not silently expire.
///
/// The transaction result is returned rather than logged away: the caller owns
/// the pending report and may only drop it once this reports success, so a
/// write that never reached the swarm is retried instead of lost. Upserting on
/// the device id also makes the retry idempotent — a redelivered report
/// overwrites the device's row rather than adding one.
pub(crate) async fn report_watchdog_reset(
    db_client: &Client,
    report: WatchdogResetReport,
) -> ZResult<()> {
    log::info!("Reporting watchdog reset to the swarm: {report:?}");
    let eid = report.device_id.clone().into_bytes();
    db_client
        .write_tx_in_with_retention(
            scope_of_watchdog_resets(),
            None,
            async move |client, tx_id| {
                client
                    .send(tb_insert::Request {
                        id: tx_id,
                        op: tb_insert::Op {
                            scope: scope_of_watchdog_resets(),
                            table: WATCHDOG_RESETS_TABLE.to_owned(),
                            eid: Some(eid),
                            value: postcard::to_allocvec(&report).map_err(|e| {
                                zerror!("failed to serialize watchdog reset report: {e}")
                            })?,
                        },
                    })
                    .await
                    .map_err(|e| zerror!("unable to report watchdog reset: {e}"))?
                    .map_err(|e| zerror!("unable to send insert request: {}", e.message))?;

                Ok(())
            },
        )
        .await
}
