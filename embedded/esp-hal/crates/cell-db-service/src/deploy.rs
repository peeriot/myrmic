use alloc::borrow::ToOwned;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::cmp::Ordering;

use cell_protocol::{
    ArtifactLocation, ArtifactPlatform, DEPLOYMENT_RESPONSES_TABLE, DEPLOYMENT_TABLE,
    DeploymentCommand, DeploymentConfirmation, Sri, scope_of_deployment,
};
use cfg_match::cfg_match;
use db_client::v1::models::{BlobResponse, Id, tb_insert};
use db_client::v1::{Client, models as db_models};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer, with_timeout};
use myrmic_common::cells::Command;
use portable_atomic_util::Arc;
use wasm_storage::__reexports::postcard;
use wasm_storage::metadata::Metadata;
use zenoh_nano::scout::ZenohIdProto;
use zenoh_result::zerror;

use wasm_runtime::{TransferReply, WasmTransfer};

/// Error encountered during deployment
#[derive(Debug, thiserror::Error)]
pub(crate) enum DeployError {
    #[error("Unable to parse metadata")]
    MetadataParse,
    #[error("Metadata could not be found")]
    MetadataNotFound,
    #[error("Db operation timed out")]
    DbTimedOut,
    #[error("Db unreachable: {0}")]
    DbUnreachable(zenoh_result::Error),
}

/// Handle deployment
pub(crate) async fn handle(
    client: &Client,
    zid: ZenohIdProto,
    last_deploy_id: &mut Option<Id>,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    cell: Option<&(Sri, Vec<Command>)>,
    awaiting_deletion_confirmation: &mut bool,
    watched: &mut Option<cell_protocol::supervision::WatchedCell>,
) {
    if let Some(cmd) = poll_for_deployment(client, zid, last_deploy_id).await {
        match cmd {
            DeploymentCommand::Deploy {
                sri,
                class,
                payload,
                gen_id,
                lineage,
            } => {
                // If by now we redeployed, we don't need to send a confirmation since
                // deployment by definition deletes the previous cell
                *awaiting_deletion_confirmation = false;

                // The verification pass fences this cell from now on.
                *watched = Some(cell_protocol::supervision::WatchedCell {
                    sri,
                    gen_id,
                    lineage: lineage.clone(),
                });
                if let Err(e) =
                    deploy(client, sri, &class, payload, gen_id, lineage, wasm_transfer).await
                {
                    log::error!("[db-client] Failed to deploy; {e}");
                }
            }
            DeploymentCommand::Delete { sri: requested_sri } => {
                if let Some((current_sri, _)) = cell {
                    // We have a running cell
                    if current_sri == &requested_sri {
                        // SRI matches, we can terminate
                        *awaiting_deletion_confirmation = true;
                        wasm_runtime::terminate_module();
                    } else {
                        log::warn!(
                            "Received request for cell deletion, but with wrong SRI. Disregarding \
                            request. Current SRI '{}', requested SRI '{}'",
                            current_sri,
                            requested_sri
                        );
                    }
                } else {
                    // No cell is running, might as well confirm immediately
                    confirm_deployment(
                        client,
                        zid,
                        DeploymentConfirmation::Deleted { sri: requested_sri },
                    )
                    .await;
                }
            }
        }
    }
}

/// Deploys a Cell onto the device.
///
/// The module transfer opens its own read transaction, and that `tx_begin` can transiently fail to
/// locate a db node ("no connected databases") when a discovery reply is dropped on the radio —
/// even though the metadata read moments earlier succeeded. A fresh attempt then finds the db, so
/// retry a few times inside the orchestrator's deploy window instead of failing the deployment on
/// the first transient miss. Only transient errors are retried; a bad/absent module fails fast.
pub(crate) async fn deploy(
    client: &Client,
    sri: Sri,
    class_name: &str,
    deployment_payload: Option<Vec<u8>>,
    gen_id: cell_protocol::Gen,
    lineage: cell_protocol::SpawnLineage,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
) -> Result<(), DeployError> {
    const MAX_ATTEMPTS: u32 = 3;
    let mut attempt = 1;
    loop {
        match deploy_once(
            client,
            sri,
            class_name,
            deployment_payload.clone(),
            gen_id,
            lineage.clone(),
            wasm_transfer,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(err)
                if attempt < MAX_ATTEMPTS
                    && matches!(err, DeployError::DbUnreachable(_) | DeployError::DbTimedOut) =>
            {
                log::warn!(
                    "[db] deploy attempt {attempt}/{MAX_ATTEMPTS} failed transiently ({err}); \
                     retrying"
                );
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

/// A single deploy attempt: fetch the metadata, then stream the module into flash.
async fn deploy_once(
    client: &Client,
    sri: Sri,
    class_name: &str,
    deployment_payload: Option<Vec<u8>>,
    gen_id: cell_protocol::Gen,
    lineage: cell_protocol::SpawnLineage,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
) -> Result<(), DeployError> {
    log::info!("[db] Requesting WASM metadata...");
    let len_to_transfer = fetch_metadata(
        client,
        sri,
        class_name,
        deployment_payload,
        gen_id,
        lineage,
        wasm_transfer,
    )
    .await?;
    if len_to_transfer == 0 {
        // Nothing to transfer (already stored)
        return Ok(());
    }

    // Wait for the runtime handler to process the message
    while wasm_transfer.is_full() {
        Timer::after_millis(100).await;
    }

    log::info!("[db] Requesting WASM module...");
    let (scope, path) = cfg_match! {
        any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61") => ArtifactLocation::aot(class_name, ArtifactPlatform::Riscv32imac).into_parts(),
        _ => compile_error!("Missing SoC variant selection");
    };
    let fetch = client.read_tx_in(scope.clone(), async move |client, id| {
        // We choose a size that doesn't impact massively on RAM
        const CHUNK: usize = 4_096;
        let mut current_offset = 0u64;

        loop {
            // Make sure we have a slot ready for the runtime handler, or the channel
            // being busy is going to cause our DB request to timeout
            while wasm_transfer.is_full() {
                Timer::after_millis(100).await;
            }

            let res = client
                .send(db_models::path_resolve::Request {
                    id,
                    op: db_models::path_resolve::Op {
                        scope: scope.clone(),
                        path: path.clone(),
                        range: Some(db_models::ChunkRange {
                            offset: current_offset,
                            length: CHUNK as u64,
                        }),
                    },
                })
                .await?
                .map_err(|e| zerror!("{}", e.message))?;

            match res.blob {
                // Make sure the response passes all our expectations
                Some(BlobResponse {
                    blob,
                    range:
                        Some(db_models::ChunkRange {
                            offset: blob_offset,
                            ..
                        }),
                    total_len,
                    ..
                }) if (1..=CHUNK).contains(&blob.len())
                    && blob_offset == current_offset
                    && total_len == u64::from(len_to_transfer) =>
                {
                    log::trace!(
                        "[db] Received chunk: offset={}, length={}",
                        blob_offset,
                        blob.len()
                    );
                    current_offset += blob.len() as u64;
                    match current_offset.cmp(&total_len) {
                        // More chunks to come
                        Ordering::Less => {
                            wasm_transfer.send(WasmTransfer::Chunk(blob)).await;
                        }
                        // This was the last chunk
                        Ordering::Equal => {
                            wasm_transfer.send(WasmTransfer::End(blob)).await;
                            break;
                        }
                        Ordering::Greater => {
                            log::error!("[db] Received more data than expected");
                            wasm_transfer.send(WasmTransfer::Abort).await;
                            break;
                        }
                    }
                }
                _ => {
                    log::error!("[db] No/Invalid blob found");
                    wasm_transfer.send(WasmTransfer::Abort).await;
                    break;
                }
            }
        }

        Ok(())
    });

    match with_timeout(Duration::from_secs(20), fetch).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => {
            wasm_transfer.send(WasmTransfer::Abort).await;

            Err(DeployError::DbUnreachable(err))
        }
        Err(_) => {
            wasm_transfer.send(WasmTransfer::Abort).await;

            Err(DeployError::DbTimedOut)
        }
    }
}

/// Polls to check whether a new deployment is available for the device
pub(crate) async fn poll_for_deployment(
    client: &Client,
    zid: ZenohIdProto,
    last_deploy_id: &mut Option<Id>,
) -> Option<DeploymentCommand> {
    let (tx_id, eid, payload) = crate::mailbox::poll_table_then_delete(
        client,
        &scope_of_deployment(zid.to_string()),
        DEPLOYMENT_TABLE,
        last_deploy_id.clone(),
    )
    .await?;
    *last_deploy_id = Some(eid);

    // Deserialize the Mailbox Command
    match postcard::from_bytes::<DeploymentCommand>(payload.as_slice()) {
        Ok(deployment_command) => {
            // Commit so the original entry can be deleted
            drop(
                client
                    .send(db_models::tx_commit::Request { id: tx_id })
                    .await,
            );

            Some(deployment_command)
        }
        Err(e) => {
            log::error!("[db-client] Failed to deserialize DeploymentCommand: {e}, {payload:?}");

            // Rollback
            drop(
                client
                    .send(db_models::tx_rollback::Request { id: tx_id })
                    .await,
            );

            None
        }
    }
}

/// Sends a deployment confirmation back to respond to the deployment command
pub(crate) async fn confirm_deployment(
    db_client: &Client,
    zid: ZenohIdProto,
    confirmation: DeploymentConfirmation,
) {
    match postcard::to_allocvec(&confirmation) {
        Ok(value) => {
            if db_client
                .write_tx_in(
                    scope_of_deployment(zid.to_string()),
                    async move |client, tx_id| {
                        client
                            .send(tb_insert::Request {
                                id: tx_id,
                                op: tb_insert::Op {
                                    scope: scope_of_deployment(zid.to_string()),
                                    table: DEPLOYMENT_RESPONSES_TABLE.to_owned(),
                                    value,
                                    eid: None,
                                },
                            })
                            .await
                            .map_err(|err| zerror!("Failed to communicate with DB: {err}"))?
                            .map_err(|err| {
                                zerror!("Failed to insert deployment confirmation: {err:?}")
                            })?;

                        Ok(())
                    },
                )
                .await
                .is_err()
            {
                log::error!("[db-client] Failed to execute DB transaction");
            }
        }
        Err(e) => {
            log::error!("[db-client] Failed to serialize deployment confirmation {e}");
        }
    }
}

/// Fetches the metadata and returns how many bytes are to be transferred
async fn fetch_metadata(
    client: &Client,
    sri: Sri,
    class_name: &str,
    deployment_payload: Option<Vec<u8>>,
    gen_id: cell_protocol::Gen,
    lineage: cell_protocol::SpawnLineage,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
) -> Result<u32, DeployError> {
    let (scope, path) = cfg_match! {
        any(feature = "esp32c5", feature = "esp32c6", feature = "esp32c61") => ArtifactLocation::meta(class_name, ArtifactPlatform::Riscv32imac).into_parts(),
        _ => compile_error!("Missing SoC variant selection");
    };
    let fetch_metadata = client.read_tx_in(scope.clone(), async move |client, id| {
        let res = client
            .send(db_models::path_resolve::Request {
                id,
                op: db_models::path_resolve::Op {
                    scope,
                    path,
                    range: None,
                },
            })
            .await?
            .map_err(|err| zenoh_result::zerror!("unable to request metadata: {}", err.message))?;

        Ok(res.blob)
    });
    match with_timeout(Duration::from_secs(20), fetch_metadata).await {
        Ok(Ok(Some(BlobResponse { blob, .. }))) => {
            let metadata: Metadata = match postcard::from_bytes(&blob) {
                Ok(metadata) => metadata,
                Err(e) => {
                    log::error!("[db] Failed to parse WASM metadata: {}", e);
                    return Err(DeployError::MetadataParse);
                }
            };
            let len = metadata.len;
            log::trace!("[db] Parsed metadata: {:?}", metadata);

            // Stop running module if it's already running, since we're about to store a new one
            wasm_runtime::terminate_module();
            let reply = Arc::new(Signal::new());
            wasm_transfer
                .send(WasmTransfer::Start {
                    metadata,
                    sri,
                    class_name: class_name.to_owned(),
                    payload: deployment_payload,
                    gen_id,
                    lineage,
                    reply: Arc::clone(&reply),
                })
                .await;
            if let TransferReply::AlreadyStored = reply.wait().await {
                log::info!("[db] WASM module already stored, skipping transfer");
                Ok(0)
            } else {
                Ok(len)
            }
        }
        Ok(Ok(None)) => Err(DeployError::MetadataNotFound),
        Ok(Err(err)) => Err(DeployError::DbUnreachable(err)),
        Err(_) => Err(DeployError::DbTimedOut),
    }
}
