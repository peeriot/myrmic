use alloc::borrow::ToOwned;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::cmp::Ordering;

use cell_protocol::{
    BlobHash, DEPLOYMENT_RESPONSES_TABLE, DEPLOYMENT_TABLE, DeploymentCommand,
    DeploymentConfirmation, Sri, class_blob_id, class_registry_scope, scope_of_deployment,
};
use db_client::v1::models::{BlobResponse, Id, tb_insert};
use db_client::v1::{Client, models as db_models};
use embassy_futures::select::{Either, select};
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
    #[error("AOT module could not be found")]
    ModuleNotFound,
    #[error("AOT module is too large to store ({0} bytes)")]
    ModuleTooLarge(u64),
    #[error("Db operation timed out")]
    DbTimedOut,
    #[error("Db unreachable: {0}")]
    DbUnreachable(zenoh_result::Error),
}

/// A deployment waiting for the cell it replaces to tear down, which needs the service loop free.
pub(crate) struct PendingDeploy {
    sri: Sri,
    module: Module,
    payload: Option<Vec<u8>>,
    gen_id: cell_protocol::Gen,
    lineage: cell_protocol::SpawnLineage,
}

/// Resolves once the runtime can take a module; re-asserts the stop, as loading a cell clears it.
pub(crate) async fn runtime_ready() {
    while !wasm_runtime::is_idle() {
        wasm_runtime::terminate_module();
        Timer::after_millis(100).await;
    }
}

/// Runs a parked deployment; the runtime must be idle (see [`runtime_ready`]).
pub(crate) async fn run_pending(
    client: &Client,
    pending: PendingDeploy,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    watched: &mut Option<cell_protocol::supervision::WatchedCell>,
) {
    let PendingDeploy {
        sri,
        module,
        payload,
        gen_id,
        lineage,
    } = pending;

    // The verification pass fences this cell from now on.
    *watched = Some(cell_protocol::supervision::WatchedCell {
        sri,
        gen_id,
        lineage: lineage.clone(),
    });
    if let Err(e) = deploy(client, sri, module, payload, gen_id, lineage, wasm_transfer).await {
        log::error!("[db-client] Failed to deploy; {e}");
    }
}

/// Handle deployment
#[allow(
    clippy::too_many_arguments,
    reason = "the whole deployment context, threaded from the one service loop that owns it"
)]
pub(crate) async fn handle(
    client: &Client,
    zid: ZenohIdProto,
    last_deploy_id: &mut Option<Id>,
    cell: Option<&(Sri, Vec<Command>)>,
    native_held: bool,
    awaiting_deletion_confirmation: &mut bool,
    watched: Option<&cell_protocol::supervision::WatchedCell>,
    pending: &mut Option<PendingDeploy>,
) {
    if let Some(cmd) = poll_for_deployment(client, zid, last_deploy_id).await {
        match cmd {
            DeploymentCommand::Deploy { sri, .. } if native_held => {
                // A node hosts one cell, and this node's firmware is it. The
                // orchestrator should not have got here — the native placement
                // row marks this node occupied — so refuse loudly rather than
                // letting a module displace the firmware's own cell.
                log::error!(
                    "[db-client] refusing to deploy '{sri}': this node's cell slot is held by \
                     its firmware"
                );
                confirm_deployment(
                    client,
                    zid,
                    DeploymentConfirmation::Deployed {
                        failure: Some(alloc::string::String::from(
                            "node's cell slot is held by native firmware",
                        )),
                        sri,
                    },
                )
                .await;
            }
            DeploymentCommand::Deploy {
                sri,
                class,
                aot,
                crc,
                payload,
                gen_id,
                lineage,
            } => {
                // Not newer than what we run: a timed-out attempt nobody awaits. A confirmation
                // would be matched by SRI against the live deploy, so send none.
                let newest = pending
                    .as_ref()
                    .map(|p| p.gen_id)
                    .max(watched.map(|w| w.gen_id));
                if let Some(newest) = newest
                    && gen_id <= newest
                {
                    log::warn!("[db-client] ignoring stale deployment of '{sri}' ({gen_id:?})");
                    return;
                }

                // If by now we redeployed, we don't need to send a confirmation since
                // deployment by definition deletes the previous cell
                *awaiting_deletion_confirmation = false;

                wasm_runtime::terminate_module();
                let superseded = pending.replace(PendingDeploy {
                    sri,
                    module: Module { class, aot, crc },
                    payload,
                    gen_id,
                    lineage,
                });
                if let Some(superseded) = superseded {
                    log::warn!(
                        "[db-client] deployment of '{}' superseded by '{sri}' before it ran",
                        superseded.sri
                    );
                }
            }
            DeploymentCommand::Delete { sri: requested_sri } => {
                // A deploy that never ran is deleted by dropping it.
                let cancelled = pending.take_if(|p| p.sri == requested_sri).is_some();

                if let Some((current_sri, _)) = cell
                    && (current_sri == &requested_sri || !cancelled)
                {
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
                    // No cell is running, or its deploy never ran: confirm immediately
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

/// The module a deploy brings onto the device: the cell's class, the id of the
/// AOT binary that runs it here, and the CRC the flashed copy must match.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Module {
    class: BlobHash,
    aot: BlobHash,
    crc: u32,
}

/// Deploys a Cell onto the device.
///
/// The module transfer opens its own read transaction, and that `tx_begin` can transiently fail to
/// locate a db node ("no connected databases") when a discovery reply is dropped on the radio —
/// even though the size read moments earlier succeeded. A fresh attempt then finds the db, so
/// retry a few times inside the orchestrator's deploy window instead of failing the deployment on
/// the first transient miss. Only transient errors are retried; a bad/absent module fails fast.
pub(crate) async fn deploy(
    client: &Client,
    sri: Sri,
    module: Module,
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
            module,
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

/// A single deploy attempt: size the module, then stream it into flash.
async fn deploy_once(
    client: &Client,
    sri: Sri,
    module: Module,
    deployment_payload: Option<Vec<u8>>,
    gen_id: cell_protocol::Gen,
    lineage: cell_protocol::SpawnLineage,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
) -> Result<(), DeployError> {
    log::info!("[db] Requesting AOT module size...");
    let len_to_transfer = start_transfer(
        client,
        sri,
        module,
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

    log::info!("[db] Requesting AOT module...");
    let blob_id = class_blob_id(module.aot);
    // Signalled on every received chunk; the transfer is aborted once it stalls for longer than
    // `CHUNK_STALL_TIMEOUT`, however long the whole transfer takes
    let progress = Signal::<CriticalSectionRawMutex, ()>::new();
    // Shadow reference so the async block can capture it without moving it
    let progress = &progress;
    let fetch = client.read_tx_in(class_registry_scope(), async move |client, id| {
        let mut current_offset = 0u64;

        while current_offset < u64::from(len_to_transfer) {
            // Make sure we have a slot ready for the runtime handler, or the channel
            // being busy is going to cause our DB request to timeout
            while wasm_transfer.is_full() {
                Timer::after_millis(100).await;
            }

            let res = client
                .send(db_models::blob_resolve::Request {
                    id,
                    op: db_models::blob_resolve::Op {
                        blob_id: blob_id.clone(),
                        range: Some(db_models::ChunkRange {
                            offset: current_offset,
                            length: CHUNK_SIZE as u64,
                        }),
                    },
                })
                .await?
                .map_err(|e| zerror!("{}", e.message))?;

            if forward_chunk(
                res.blob,
                &mut current_offset,
                len_to_transfer,
                wasm_transfer,
                progress,
            )
            .await
            .is_err()
            {
                break;
            }
        }

        Ok(())
    });

    let stall = async {
        while with_timeout(CHUNK_STALL_TIMEOUT, progress.wait())
            .await
            .is_ok()
        {}
    };

    match select(fetch, stall).await {
        Either::First(Ok(())) => Ok(()),
        Either::First(Err(err)) => {
            wasm_transfer.send(WasmTransfer::Abort).await;

            Err(DeployError::DbUnreachable(err))
        }
        Either::Second(()) => {
            log::warn!("[db] No WASM chunk received for {CHUNK_STALL_TIMEOUT:?}");
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

/// Sizes the module, hands its metadata to the runtime handler and returns how
/// many bytes are to be transferred: 0 if the module is already stored.
///
/// The metadata is assembled here rather than fetched: its length is the blob's
/// size, and its CRC and hash come with the deployment command (the hash is the
/// module's id).
async fn start_transfer(
    client: &Client,
    sri: Sri,
    module: Module,
    deployment_payload: Option<Vec<u8>>,
    gen_id: cell_protocol::Gen,
    lineage: cell_protocol::SpawnLineage,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
) -> Result<u32, DeployError> {
    let size = client.read_tx_in(class_registry_scope(), async move |client, id| {
        let res = client
            .send(db_models::blob_resolve::Request {
                id,
                op: db_models::blob_resolve::Op {
                    blob_id: class_blob_id(module.aot),
                    // An empty range returns no bytes, only the blob's size.
                    range: Some(db_models::ChunkRange {
                        offset: 0,
                        length: 0,
                    }),
                },
            })
            .await?
            .map_err(|err| zenoh_result::zerror!("unable to size module: {}", err.message))?;

        Ok(res.blob.map(|blob| blob.total_len))
    });
    match with_timeout(Duration::from_secs(20), size).await {
        Ok(Ok(Some(total_len))) => {
            let len =
                u32::try_from(total_len).map_err(|_| DeployError::ModuleTooLarge(total_len))?;
            let BlobHash::Sha2(hash) = module.aot;
            let metadata = Metadata {
                magic: wasm_storage::metadata::MAGIC,
                version: wasm_storage::metadata::VERSION,
                len,
                crc: module.crc,
                hash,
            };
            log::trace!("[db] Module metadata: {:?}", metadata);

            // Stop running module if it's already running, since we're about to store a new one
            wasm_runtime::terminate_module();
            let reply = Arc::new(Signal::new());
            wasm_transfer
                .send(WasmTransfer::Start {
                    metadata,
                    sri,
                    class: module.class,
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
        Ok(Ok(None)) => Err(DeployError::ModuleNotFound),
        Ok(Err(err)) => Err(DeployError::DbUnreachable(err)),
        Err(_) => Err(DeployError::DbTimedOut),
    }
}

/// Validates a received module chunk and forwards it to the runtime handler. Fails if the chunk
/// is invalid, in which case the transfer has been aborted
async fn forward_chunk(
    blob: Option<BlobResponse>,
    current_offset: &mut u64,
    len_to_transfer: u32,
    wasm_transfer: Sender<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    progress: &Signal<CriticalSectionRawMutex, ()>,
) -> Result<(), ()> {
    match blob {
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
        }) if (1..=CHUNK_SIZE).contains(&blob.len())
            && blob_offset == *current_offset
            && total_len == u64::from(len_to_transfer) =>
        {
            log::trace!(
                "[db] Received chunk: offset={}, length={}",
                blob_offset,
                blob.len()
            );
            progress.signal(());
            *current_offset += blob.len() as u64;
            match (*current_offset).cmp(&total_len) {
                // More chunks to come
                Ordering::Less => {
                    wasm_transfer.send(WasmTransfer::Chunk(blob)).await;

                    Ok(())
                }
                // This was the last chunk
                Ordering::Equal => {
                    wasm_transfer.send(WasmTransfer::End(blob)).await;

                    Ok(())
                }
                Ordering::Greater => {
                    log::error!("[db] Received more data than expected");
                    wasm_transfer.send(WasmTransfer::Abort).await;

                    Err(())
                }
            }
        }
        _ => {
            log::error!("[db] No/Invalid blob found");
            wasm_transfer.send(WasmTransfer::Abort).await;

            Err(())
        }
    }
}

/// Size of a module chunk requested from the DB, chosen so it doesn't impact massively on RAM
const CHUNK_SIZE: usize = 4_096;

/// How long the module transfer may go without receiving a chunk before it is aborted
const CHUNK_STALL_TIMEOUT: Duration = Duration::from_secs(10);
