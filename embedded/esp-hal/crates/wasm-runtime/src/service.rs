//! WASM module lifecycle: the chunked transfer protocol, the storage-to-runtime
//! handler, the cell-message pump and the blocking WAMR runtime thread.
//!
//! The async entry points are plain `async fn`s — the firmware binary wraps
//! them in its own `#[embassy_executor::task]`s and does all spawning, so the
//! embassy-executor version remains a firmware-side choice. The `liveness`
//! hooks are invoked once per loop iteration for the binary's task-liveness
//! accounting.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::ptr::addr_of;

use cell_protocol::Sri;
use embassy_futures::yield_now;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Receiver;
use embassy_sync::signal::Signal;
use esp_radio_rtos_driver::queue::QueueHandle;
use portable_atomic_util::Arc;
use static_cell::StaticCell;
use wasm_storage::WasmStorage;
use wasm_storage::metadata::Metadata;

#[cfg(feature = "ble")]
use crate::async_request::Ble;
use crate::async_request::{CELL_MSG_CHANNEL, DbClient, send_request_and_wait};
use crate::{CellMessage, TimerCompletionGuard, WamrRuntime, is_terminated};

/// Moves a value across an `esp_radio_rtos_driver` thread spawn, through the
/// entry point's `*mut c_void` argument. The `Send` bound restores the check
/// the raw pointer erases: a `!Send` value (e.g. anything built on
/// `NoopRawMutex`) must not reach another thread's executor.
pub fn into_thread_arg<T: Send>(value: T) -> *mut c_void {
    Box::into_raw(Box::new(value)).cast()
}

/// Reclaims a value handed over via [`into_thread_arg`].
///
/// # Safety
/// `arg` must come from `into_thread_arg::<T>` and be consumed exactly once.
pub unsafe fn from_thread_arg<T: Send>(arg: *mut c_void) -> T {
    // SAFETY: caller guarantees `arg` is the `Box::into_raw` of a `T`.
    *unsafe { Box::from_raw(arg.cast::<T>()) }
}

/// Messages for transferring a chunked WASM module data
pub enum WasmTransfer {
    /// Start of transfer, with the module's metadata
    Start {
        /// Module's metadata
        metadata: Metadata,
        /// Deployment SRI
        sri: Sri,
        /// Class Name
        class_name: String,
        /// Deployment payload (used as args for init)
        payload: Option<Vec<u8>>,
        /// Generation minted for this deploy: the body's fencing identity
        gen_id: cell_protocol::Gen,
        /// The spawn edge the cell is born on (parent, detachment, grace)
        lineage: cell_protocol::SpawnLineage,
        /// Reply signal where we expect a [`TransferReply`] to tell us if to proceed or if this
        /// module is already present on the device
        reply: Arc<Signal<CriticalSectionRawMutex, TransferReply>>,
    },
    /// Chunk of data
    Chunk(Vec<u8>),
    /// Final chunk of data
    End(Vec<u8>),
    /// Abort transfer
    Abort,
}

impl core::fmt::Debug for WasmTransfer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Start { metadata, sri, .. } => f
                .debug_struct("Start")
                .field("metadata", metadata)
                .field("sri", sri)
                .finish_non_exhaustive(),
            Self::Chunk(data) => f.debug_tuple("Chunk").field(&data.len()).finish(),
            Self::End(data) => f.debug_tuple("End").field(&data.len()).finish(),
            Self::Abort => write!(f, "Abort"),
        }
    }
}

/// A reply to `WasmTransfer::Start`
#[derive(Debug)]
pub enum TransferReply {
    /// Reply to proceed with the transfer of the module
    Proceed,
    /// Reply given if the same WASM module is already present in storage
    AlreadyStored,
}

/// Signal that is used to notify the Async side that the WAMR runtime has completed the execution
/// of the WASM module (regardless if completed, aborted or errored)
static WAMR_DONE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// The item shape carried (as a raw `Box` pointer) on the module queue.
type ModuleItem = (
    *const u8,
    usize,
    Sri,
    String,
    cell_protocol::Gen,
    cell_protocol::SpawnLineage,
    Option<Vec<u8>>,
);

/// Gathers the system's data and provides back-pressure via embassy channels to the RTOS queue.
pub async fn cell_pump(cell_message_queue: &'static QueueHandle, liveness: fn()) {
    log::info!("Starting Cell task");

    loop {
        // Liveness (observed): blocks on an empty cell channel.
        liveness();

        let msg = CELL_MSG_CHANNEL.receive().await;
        let boxed = Box::new(msg);
        let ptr = Box::into_raw(boxed);
        // safety: Made sure already the type is correct in `WamrContext`
        unsafe {
            // Use deadline, so that we can avoid deadlocking the embassy executor if the Cell is
            // waiting on us
            loop {
                // The cell is tearing down; forwarding now would misdeliver this message
                // to whatever cell deploys next, so drop it instead of pushing.
                if is_terminated() {
                    drop(Box::from_raw(ptr));
                    break;
                }
                if cell_message_queue.send_to_back(addr_of!(ptr).cast(), Some(1_000)) {
                    break;
                }
                yield_now().await;
            }
        }
    }
}

/// Creates the WAMR queue plumbing and starts the blocking WAMR runtime on its
/// own RTOS thread.
///
/// # Returns
/// The module queue (fed by [`runtime_handler`]) and the cell-message queue
/// (fed by [`cell_pump`]).
///
/// # Panics
/// Panics when called a second time (the underlying queues are one-shot
/// statics).
pub fn start_runtime_thread(
    priority: u32,
    stack_size: usize,
) -> (&'static QueueHandle, &'static QueueHandle) {
    let (context, module_queue, cell_message_queue) = WamrContext::new();
    // SAFETY: `wamr_runtime` is a valid `extern "C"` entry point; its argument
    // is the boxed `WamrContext` whose ownership moves to the thread, and the
    // one-shot statics inside `WamrContext::new` ensure a single spawn.
    unsafe {
        esp_radio_rtos_driver::task_create(
            "WAMR Runtime",
            wamr_runtime,
            context.cast(),
            priority,
            None,
            stack_size,
        );
    }
    (module_queue, cell_message_queue)
}

/// A context for the WAMR runtime that simplifies (hides) the ugly creation of OS queues
struct WamrContext {
    /// Queue used to load and execute WASM modules
    module_queue: &'static QueueHandle,
    /// Queue used to deliver Cell Messages
    cell_message_queue: &'static QueueHandle,
}

impl core::fmt::Debug for WamrContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WamrContext").finish_non_exhaustive()
    }
}

impl WamrContext {
    /// Creates a context for the WAMR runtime
    ///
    /// # Returns
    /// (
    ///     *mut Self, // Raw pointer to the context (to be passed to the WAMR runtime task)
    ///     &'static [`QueueHandle`], // Queue for WASM modules
    ///     &'static [`QueueHandle`], // Queue for Cell Messages
    /// )
    ///
    /// # Panics
    ///
    /// Panics when called a second time (the underlying queues are one-shot statics).
    #[must_use]
    fn new() -> (*mut Self, &'static QueueHandle, &'static QueueHandle) {
        static MODULE_QUEUE: StaticCell<QueueHandle> = StaticCell::new();
        static CELL_MESSAGE_QUEUE: StaticCell<QueueHandle> = StaticCell::new();
        let module_queue =
            MODULE_QUEUE.init(QueueHandle::new(1, core::mem::size_of::<*mut ModuleItem>()));
        let cell_message_queue = CELL_MESSAGE_QUEUE.init(QueueHandle::new(
            1,
            core::mem::size_of::<*mut CellMessage>(),
        ));

        (
            Box::into_raw(Box::new(Self {
                module_queue,
                cell_message_queue,
            })),
            module_queue,
            cell_message_queue,
        )
    }

    /// Reconstructs the context from a raw Boxed pointer
    unsafe fn from_box_ptr(ptr: *mut Self) -> Self {
        assert!(!ptr.is_null());

        // safety: We checked that is non-null
        unsafe { *Box::from_raw(ptr) }
    }

    /// Waits indefinitely for a new WASM module
    fn wait_for_module(
        &self,
    ) -> (
        &[u8],
        Sri,
        String,
        cell_protocol::Gen,
        cell_protocol::SpawnLineage,
        Option<Vec<u8>>,
    ) {
        let mut ptr_out = MaybeUninit::<*mut ModuleItem>::uninit();

        // safety: We made sure that the queue size was initialized in `new()` with the correct type
        unsafe {
            // Block forever (until we get a module)
            // safety: No timeout = always true
            assert!(self.module_queue.receive(ptr_out.as_mut_ptr().cast(), None));
        }
        // Reconstruct the Box and take ownership of it
        // safety: We initialized it with what we received from the queue
        let ptr = unsafe { ptr_out.assume_init() };
        // safety: We own the Box now and there's no other pathway that takes ownership of this object
        let (data, len, sri, class_name, gen_id, lineage, payload) = unsafe { *Box::from_raw(ptr) };
        // safety: This matches the signature of the Box that was sent
        (
            unsafe { core::slice::from_raw_parts(data, len) },
            sri,
            class_name,
            gen_id,
            lineage,
            payload,
        )
    }

    /// Waits indefinitely for a new [`CellMessage`] (coming from the [`cell_pump`])
    fn wait_for_cell_message(&self) -> CellMessage {
        // A pending stop must take priority over queued work. The timeout branch
        // below only notices termination when the queue drains, so a producer
        // that keeps it non-empty — a BLE scan streaming `discovered` callbacks —
        // would otherwise starve teardown and the cell could never be stopped.
        if is_terminated() {
            return CellMessage::Destroy;
        }

        let mut ptr_out = MaybeUninit::<*mut CellMessage>::uninit();

        // safety: We made sure that the queue size was initialized in `new()` with the correct type
        unsafe {
            // Block until we get a message (or check every 500ms if we are supposed to be
            // terminated)
            loop {
                let did_receive = self
                    .cell_message_queue
                    .receive(ptr_out.as_mut_ptr().cast(), Some(500_000));
                if did_receive {
                    break;
                } else if is_terminated() {
                    return CellMessage::Destroy;
                }
            }
        }
        // Reconstruct the Box and take ownership of it
        // safety: We initialized it with what we received from the queue
        let ptr = unsafe { ptr_out.assume_init() };
        // safety: We own the Box now and there's no other pathway that takes ownership of this object
        unsafe { *Box::from_raw(ptr) }
    }

    /// Drains any [`CellMessage`]s still sitting in the RTOS queue, reconstructing and
    /// dropping each leaked `Box`. Paired with the `is_terminated()` gate in [`cell_pump`]
    /// (which stops feeding this queue during teardown), this keeps a message queued for a
    /// torn-down cell from being misdelivered to the next one — and from leaking.
    fn drain_cell_messages(&self) {
        let mut ptr_out = MaybeUninit::<*mut CellMessage>::uninit();
        // safety: We made sure that the queue size was initialized in `new()` with the correct type
        while unsafe {
            self.cell_message_queue
                .receive(ptr_out.as_mut_ptr().cast(), Some(0))
        } {
            // safety: We initialized it with what we received from the queue
            let ptr = unsafe { ptr_out.assume_init() };
            // safety: We own the Box now and there's no other pathway that takes ownership of this object
            drop(unsafe { Box::from_raw(ptr) });
        }
    }
}

/// The dynamic storing/loading/unloading of WASM modules between the storage and the WASM runtime.
pub async fn runtime_handler(
    mut wasm_storage: WasmStorage<CriticalSectionRawMutex>,
    wasm_file_transfer: Receiver<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    module_queue: &'static QueueHandle,
    liveness: fn(),
) {
    log::info!("Starting runtime handler");

    let mut metadata_on_device = wasm_storage.load().map(|(metadata, _)| metadata);
    'wait_for_module: loop {
        // Liveness (observed): blocks awaiting a deployment.
        liveness();

        // Because we don't support autoloading/deployment of cell, we first await a new deployment
        // before we attempt to load from storage
        log::info!("Waiting for new incoming module");
        // Load the next incoming module
        let sri;
        let class_name;
        let payload;
        let gen_id;
        let lineage;
        let new_metadata = loop {
            if let WasmTransfer::Start {
                metadata: new_metadata,
                sri: new_sri,
                class_name: new_class_name,
                payload: new_payload,
                gen_id: new_gen_id,
                lineage: new_lineage,
                reply,
            } = wasm_file_transfer.receive().await
            {
                sri = new_sri;
                class_name = new_class_name;
                payload = new_payload;
                gen_id = new_gen_id;
                lineage = new_lineage;
                if let Some(loaded_metadata) = metadata_on_device.clone()
                    && loaded_metadata == new_metadata
                {
                    // No retransfer is necessary
                    log::info!("WASM module is already stored on device.");
                    reply.signal(TransferReply::AlreadyStored);
                    // We have set the new SRI, just skip the writer and continue loading
                    break None;
                }
                // Transfer new module
                log::info!("Received new WASM module metadata: {new_metadata:?}");
                reply.signal(TransferReply::Proceed);
                break Some(new_metadata);
            }
        };

        // Write to flash and commit, if we have to write the new module (i.e. if the metadata
        // differed or if there was no module before)
        if let Some(metadata) = new_metadata
            && !store_module(&mut wasm_storage, &wasm_file_transfer, metadata).await
        {
            continue 'wait_for_module;
        }

        log::info!("Loading WASM module from storage");
        if let Some((loaded_metadata, loaded_module)) = wasm_storage.load() {
            log::info!("WASM module found. Loading into WAMR...");
            metadata_on_device = Some(loaded_metadata);
            let module_slice = loaded_module.slice();
            let module = Box::into_raw(Box::new((
                module_slice.as_ptr(),
                module_slice.len(),
                sri,
                class_name,
                gen_id,
                lineage,
                payload,
            )));
            // Send module to WAMR runtime (which will start it)
            // safety: `module_queue` was created in `new()` with size_of::<*mut ModuleItem>(),
            // so it stores only the raw pointer (pointer size is independent of the ModuleItem
            // layout). We copy exactly that pointer here; `wait_for_module` reads it back as
            // `*mut ModuleItem` and reconstructs the Box.
            unsafe {
                module_queue.send_to_back(core::ptr::addr_of!(module).cast(), None);
            }

            // Wait for completion of WAMR before we can do anything else (e.g. load a new module)
            WAMR_DONE.wait().await;
            log::info!("WAMR runtime finished");
        } else {
            log::warn!("No WASM module found!");
            metadata_on_device = None;
        }
    }
}

/// Streams the incoming module chunks into the flash writer.
/// Returns `false` if the transfer was aborted.
async fn store_module(
    wasm_storage: &mut WasmStorage<CriticalSectionRawMutex>,
    wasm_file_transfer: &Receiver<'static, CriticalSectionRawMutex, WasmTransfer, 1>,
    metadata: Metadata,
) -> bool {
    log::info!("Starting WASM writer");
    let mut writer = wasm_storage.writer(metadata);
    loop {
        match wasm_file_transfer.receive().await {
            #[expect(
                clippy::expect_used,
                reason = "Unrecoverable if we can't write to flash"
            )]
            WasmTransfer::Chunk(chunk) => {
                writer
                    .write(chunk.as_slice())
                    .expect("Failed to write chunk");
            }
            WasmTransfer::End(last_chunk) => {
                #[expect(
                    clippy::expect_used,
                    reason = "Unrecoverable if we can't write to flash"
                )]
                writer
                    .final_write(last_chunk.as_slice())
                    .expect("Failed to write last chunk");
                log::info!("Stored new WASM module in Flash");

                return true;
            }
            WasmTransfer::Abort => {
                log::warn!("WASM transfer aborted");
                return false;
            }
            #[expect(
                clippy::unreachable,
                reason = "The deploy logic prevents a re-start of a transfer without \
                aborting it first. If this fires, it's a logic bug."
            )]
            WasmTransfer::Start { .. } => {
                unreachable!("BUG: Unexpected WASM transfer message during transfer");
            }
        }
    }
}

/// Runs the WAMR runtime in a blocking manner
///
/// # Arguments
///
/// Takes an `esp-rtos` `QueueHandle` which is used to receive AOT modules in the form of slices
///
/// # Panics
///
/// Panics when handed a null context pointer.
// This is expressed as an extern "C" because instead of using embassy, the firmware uses
// `esp-rtos` to spawn the task
extern "C" fn wamr_runtime(wamr_context_box: *mut c_void) {
    // Reconstruct the WAMR Context by regaining the ownership of the raw Box
    assert!(!wamr_context_box.is_null());
    // safety: We validated that is non-null and WamrContext takes care of being correct
    let ctx = unsafe { WamrContext::from_box_ptr(wamr_context_box.cast()) };

    loop {
        log::info!("[WAMR] Waiting for WASM module");
        let (module_bytes, sri, class_name, gen_id, lineage, payload) = ctx.wait_for_module();
        log::info!("[WAMR] Initializing runtime");
        let runtime = match WamrRuntime::init_with_module(
            module_bytes,
            &sri,
            &class_name,
            gen_id,
            &lineage,
            payload,
        ) {
            Ok(runtime) => runtime,
            Err(e) => {
                log::error!("[WAMR] Runtime initialization failed: {e}");
                send_request_and_wait(DbClient::ConfirmDeployment {
                    sri,
                    available_commands: vec![],
                    failure: Some(e.to_string()),
                });
                // Signal that we are ready for another module if the initialization of the last one
                // failed
                WAMR_DONE.signal(());
                continue;
            }
        };

        // Process cell until destroyed
        log::info!("[WAMR] Cell awaiting messages");
        loop {
            let msg = ctx.wait_for_cell_message();
            log::info!("[WAMR] Received Cell message: {msg:?}");
            match msg {
                CellMessage::Command {
                    command,
                    payload,
                    sender,
                    origin,
                } => {
                    log::info!("[WAMR] Received Cell Command message");
                    if let Err(e) = runtime.handle_command(&command, payload, sender, origin) {
                        log::error!("Failed to run Cell command: {e}");
                    }
                }
                CellMessage::Event {
                    event,
                    payload,
                    sender,
                } => {
                    log::info!("[WAMR] Received Cell Event message");
                    if let Err(e) = runtime.handle_event(&event, payload, sender) {
                        log::error!("Failed to run Cell event: {e}");
                    }
                }
                CellMessage::TimerTick {
                    export_name,
                    completed,
                } => {
                    let _guard = completed.map(TimerCompletionGuard);
                    if let Err(e) = runtime.handle_timer_tick(&export_name) {
                        log::error!("Timer tick error for '{export_name}': {e}");
                    }
                }
                #[cfg(feature = "ble")]
                CellMessage::BleCallback {
                    export_name,
                    payload,
                } => {
                    log::info!("[WAMR] Received Cell BLE callback message");
                    if let Err(e) = runtime.handle_ble_callback(&export_name, payload) {
                        log::error!("BLE callback error for '{export_name}': {e}");
                    }
                }
                CellMessage::Destroy => {
                    // The cell owned whatever BLE state it left behind (an active scan/connection, subscriptions); wipe
                    // it back to idle so the next cell sees a deterministic radio, not this one's leftovers.
                    #[cfg(feature = "ble")]
                    if let Err(e) = send_request_and_wait(Ble::Reset) {
                        log::error!("[ble] reset on cell teardown failed: {e}");
                    }
                    // Drop any message still queued for the cell that is now gone (BLE callback, command, event, or
                    // timer tick alike), so it can't be misdelivered to whatever cell deploys next. Both buffers
                    // between the producer and here must be cleared: the embassy channel and the RTOS queue.
                    CELL_MSG_CHANNEL.clear();
                    ctx.drain_cell_messages();
                    send_request_and_wait(DbClient::ConfirmDeletion);
                    break;
                }
            }
        }

        // Let the runtime handler know that we are done and we can be sent a new module if needed
        WAMR_DONE.signal(());
    }
}
