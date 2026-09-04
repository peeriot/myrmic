//! WASM Runtime
//!
//! This is what can execute the WASM modules in practice
#![no_std]
#![warn(missing_debug_implementations)]
#![warn(unreachable_pub)]
#![warn(clippy::must_use_candidate)]
#![warn(clippy::return_self_not_must_use)]
#![expect(
    clippy::cast_possible_truncation,
    reason = "Needed to interface with C FFI"
)]

extern crate alloc;

mod cell;
mod macros;

pub mod async_request;
mod exports;
mod imports;
mod service;

pub use service::{
    TransferReply, WasmTransfer, cell_pump, from_thread_arg, into_thread_arg, runtime_handler,
    start_runtime_thread,
};

/// Install the tap registry for use by the "tap" WAMR host calls.
///
/// Called exclusively from the generated `pipeline_config::setup_tap_registry()`.
/// Must complete before `wasm_task()` is spawned.
#[cfg(feature = "signal-layer")]
pub fn init_tap_registry(registry: signal_layer_core::TapRegistry) {
    imports::tap::init(registry);
}

/// Install the outlet registry for use by the "outlet" WAMR host calls.
///
/// Called exclusively from the generated `pipeline_config::setup_outlet_registry()`.
/// Must complete before `wasm_task()` is spawned.
#[cfg(feature = "signal-layer")]
pub fn init_outlet_registry(registry: signal_layer_core::OutletRegistry) {
    imports::outlet::init(registry);
}

/// Install the wall-clock source for the "time" WAMR host calls: swarm-synced
/// time since `UNIX_EPOCH`, or `None` while unsynced. Until installed (and
/// while it returns `None`), `now_host` answers `EAGAIN`, so a late init is
/// safe.
pub fn init_wall_clock(clock: fn() -> Option<core::time::Duration>) {
    imports::time::init(clock);
}

/// The self-test wedge mode requested by a guest via the `selftest` host import
/// (`0` = none). The firmware's stats task polls this to trigger a deliberate
/// liveness stall for watchdog HIL tests. `wdt-selftest` feature only.
#[cfg(feature = "wdt-selftest")]
#[must_use]
pub fn wdt_selftest_wedge_mode() -> u8 {
    imports::selftest::wedge_mode()
}

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};
use core::sync::atomic::Ordering;

use cell_protocol::{
    CellInstance, Gen, INSTANCE_REGISTRY_TABLE, MESSAGES_TABLE, SpawnLineage, Sri,
    instance_registry_scope, scope_of_cell,
};
use db_client::v1::models::{Id, Scope, tb_append, tb_delete, tb_get};
use myrmic_common::cells::{Command, Event};
use portable_atomic::AtomicBool;
use wamr_rust_sdk::RuntimeError;
use wamr_rust_sdk::function::Function;
use wamr_rust_sdk::instance::Instance;
use wamr_rust_sdk::module::Module;
use wamr_rust_sdk::runtime::Runtime;
use wamr_rust_sdk::sys;
use wamr_rust_sdk::sys::wasm_exec_env_t;
use wamr_rust_sdk::value::WasmValue;

use crate::async_request::cell_host::CellHost;
use crate::async_request::db::DbClient;
use crate::async_request::send_request_and_wait;
use crate::imports::Imports;

// Re-export so the macros can use them
#[doc(hidden)]
pub mod __reexports {
    pub use esp_hal;
}
pub use async_request::timers::TimerCompletionGuard;
pub use cell::{CellMessage, CommandOrigin};
pub use imports::gpio::Pins;

/// Magic number used to recognize a WAMR AOT module
const AOT_MAGIC: u32 = 0x746f_6100;

/// Holds whether the WASM module should be terminated
static TERMINATE: AtomicBool = AtomicBool::new(false);

/// Runtime Errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid AOT module")]
    Aot,
    #[error("Failed to initialize runtime")]
    Init,
    #[error("Failed to setup imports")]
    Import,
    #[error("Failed to load module: {0}")]
    Load(RuntimeError),
    #[error("Failed to register commands and events")]
    CommandEventRegistration,
    #[error("Failed to run guest function: {0}")]
    GuestFunction(RuntimeError),
    #[error("Failed to run guest function: {function}; exception:{exception}")]
    GuestFunctionException { function: String, exception: String },
    #[error("Failed to deploy cell: {0}")]
    CellDeployment(String),
    #[error("Failed to register: {0}")]
    CellRegistration(String),
    #[error("Failed to instantiate module: {0}")]
    Instantiation(RuntimeError),
    #[error("Error of cell while handling command {command}: {err_msg}")]
    Command { command: String, err_msg: String },
    #[error("Error of cell while handling event {event}: {err_msg}")]
    Event { event: String, err_msg: String },
}

/// Signals the runtime that it should terminate the WASM module as soon as possible.
pub fn terminate_module() {
    log::trace!("Received request to terminate WASM module");
    TERMINATE.store(true, Ordering::Release);
}

/// Returns whether the runtime has been terminated
pub fn is_terminated() -> bool {
    TERMINATE.load(Ordering::Acquire)
}

/// A WAMR runtime that uses a WASM module to support the Cell Architecture.
///
/// `'buf` tracks the AOT byte-slice that WAMR retains a pointer to during XIP execution.
/// Fields are dropped in declaration order; the ordering below (instance → _module → _runtime)
/// satisfies WAMR's C-level destruction requirements.
#[derive(Debug)]
pub struct WamrRuntime<'buf> {
    instance: Instance<'static>,
    _module: Module<'buf>,
    _imports: Imports,
    _runtime: Runtime,
}

impl<'buf> WamrRuntime<'buf> {
    /// Initializes the WAMR runtime and loads a Cell according to the Cell Architecture
    #[expect(clippy::missing_panics_doc, reason = "Panic is unreachable")]
    pub fn init_with_module(
        aot_module: &'buf [u8],
        sri: &Sri,
        class_name: &str,
        gen_id: Gen,
        lineage: &SpawnLineage,
        payload: Option<Vec<u8>>,
    ) -> Result<Self, Error> {
        // Aux stack size (guest linear-memory call stack). Must match the region the guest
        // linker reserves via `-zstack-size` (32 KB, see sdk/.cargo/config.toml) so the
        // guest can use the full reserved region.
        const AUX_STACK_SIZE: u32 = 32 * 1024;

        log::trace!("Init of the WAMR engine");
        // Reset important states
        TERMINATE.store(false, Ordering::Release);
        // Set the stack boundary
        let stack_start = 0u8;
        let start_address = &raw const stack_start as usize;
        sys::register_stack_boundary(start_address);
        let runtime = Runtime::builder()
            .use_system_allocator()
            .build()
            .map_err(|_err| Error::Init)?;
        log::info!("WAMR engine initialized");

        let imports = imports::setup()?;

        // Validate and load the AOT module.
        log::trace!("AOT file size: {} bytes", aot_module.len());
        if aot_module.len() < 16 {
            log::error!("AOT module too small");
            return Err(Error::Aot);
        }
        #[expect(clippy::unwrap_in_result, clippy::expect_used, reason = "Unreachable")]
        let magic = u32::from_le_bytes(aot_module[0..4].try_into().expect("length checked above"));
        if magic != AOT_MAGIC {
            log::error!("AOT magic mismatch. Got: 0x{magic:08x} (expected: {AOT_MAGIC:08x})");
            return Err(Error::Aot);
        }
        let version =
            u32::from_le_bytes([aot_module[4], aot_module[5], aot_module[6], aot_module[7]]);
        log::trace!("AOT version: {version}");
        // SAFETY: `Module::from_buf` ties the returned `Module<'_>` to both `&runtime` and
        // `aot_module` via a single phantom lifetime. We need to track only the `aot_module`
        // borrow (WAMR holds a C pointer to the buffer for the lifetime of the module), while
        // the `runtime` ordering invariant is enforced structurally by `WamrRuntime`'s field
        // drop order (`_module` is dropped before `_runtime`). `Module<'_>` contains only a
        // raw C handle (`wasm_module_t`) and `PhantomData` — no Rust reference to `runtime` is
        // stored — so shortening the phantom lifetime to cover only `aot_module` is sound.
        let module: Module<'_> = unsafe {
            core::mem::transmute(
                Module::from_buf(&runtime, aot_module, "module").map_err(Error::Load)?,
            )
        };
        log::info!("WASM Module loaded");

        // Instantiate the module. Instance::new_with_args ties the instance to the module's phantom
        // lifetime. Drop ordering (instance before _module) enforces the C-side invariant, so we
        // transmute Instance to 'static to break the phantom chain.
        // SAFETY: Instance<'_> holds only a raw C handle (wasm_module_inst_t) + PhantomData.
        let instance: Instance<'static> = unsafe {
            core::mem::transmute(
                Instance::new_with_args(&runtime, &module, AUX_STACK_SIZE, 0)
                    .map_err(Error::Instantiation)?,
            )
        };
        log::info!("WASM Module instantiated");

        // Call init functions if they exist
        call_if_present_on(&instance, "init_allocator", &vec![])?;

        // Make sure that before we call `init_cell` the SRI is set or some operations might use a
        // bad or stale SRI
        send_request_and_wait(CellHost::SetSri { sri: *sri });

        // Scan the module's exports up front and populate the command registry before running
        // `init_cell`. Timers created in `#[init]` need to validate that their target command
        // exists, so the registry must be known by then.
        let (events, available_commands) = exports::get_cell_events_commands(&module);
        send_request_and_wait(CellHost::SetAvailableCommands {
            commands: available_commands.clone(),
        });

        // Ahead of the application, and in a transaction of its own: this reads
        // `sorg`, which the deploy's application is not routed to.
        let already_registered = is_sri_already_registered(sri)?;

        // One application spans the whole of init: whatever init writes, and
        // the instance row itself. An admitted deploy is a new life, and
        // supervision reads that row's generation and lineage, so the row must
        // land with — and only with — the state init wrote. Committed before
        // the deployment is confirmed, so the orchestrator never sees a
        // confirmed cell without its row.
        open_application(scope_of_cell(*sri));

        let init_result = run_cell_init(
            &instance,
            InitTarget {
                sri,
                class_name,
                gen_id,
                lineage,
            },
            payload,
            already_registered,
        );

        if let Err(err) = init_result {
            rollback_application();
            return Err(err);
        }

        commit_application().map_err(|_| Error::Init)?;

        // Register Cell in the registry
        log::info!("Registering the Cell");

        // Auto-subscribe the cell to every event it exports an `event_*` handler for.
        for event in events {
            log::info!("Subscribing cell to event: {}", event.as_ref());
            send_request_and_wait(DbClient::SubscribeEvent(event));
        }

        send_request_and_wait(CellHost::DeployCell)
            .map_err(|err| Error::CellDeployment(err.to_string()))?;

        Ok(Self {
            instance,
            _module: module,
            _imports: imports,
            _runtime: runtime,
        })
    }

    /// Handles a call to one of the cell's `command_*` handlers — one delivered
    /// from the mailbox, or one the runtime raised for the cell itself (a BLE
    /// result landing on the callback it registered). Either way the call runs
    /// with lazy transaction semantics; for a mailbox command the removal that
    /// consumes it rides the same transaction as the handler's work.
    ///
    /// `origin` is held until the call is fully closed out, so the poller waiting
    /// on its completion guard is released only once the transaction has been
    /// committed or rolled back.
    pub fn handle_command(
        &self,
        command: &Command,
        payload: Option<Vec<u8>>,
        sender: Option<Sri>,
        origin: CommandOrigin,
    ) -> Result<(), Error> {
        let command_error = |err_msg: String| Error::Command {
            command: command.as_ref().to_owned(),
            err_msg,
        };

        // Make sure that the command exists
        if !send_request_and_wait(CellHost::CommandExists(command.clone())) {
            // A command this cell does not serve can never succeed, and nothing
            // ran that needs undoing — consume it rather than leave it
            // redelivering forever.
            if let Some(msg_id) = origin.mailbox_entry() {
                open_application(scope_of_cell(send_request_and_wait(CellHost::GetSri)));

                let dropped = consume_mailbox_entry(msg_id).and_then(|()| commit_application());
                if let Err(err) = dropped {
                    log::warn!("unable to drop a command this cell does not serve: {err}");
                }
            }

            return Err(command_error("Command not found".to_owned()));
        }

        let expanded_name = format!("command_{}", command.as_ref());
        let result = self.call_in_transaction(
            &expanded_name,
            &Self::handler_args(payload, sender),
            origin.mailbox_entry(),
        );

        // Commands are fire-and-forget: there is nobody to hand the error back
        // to, so log it with the cell's identity and let the rolled-back
        // transaction — and the command staying queued — be the real consequence.
        if let Err(ref err_msg) = result {
            let sri = send_request_and_wait(CellHost::GetSri);
            log::warn!(
                "cell {sri} errored out while serving command {c}: {err_msg}",
                c = command.as_ref()
            );
        }

        // The transaction is closed, so the poller may hand over the next command.
        drop(origin);

        result.map_err(command_error)
    }

    /// Delivers a BLE result to the cell. The callback the cell registered is one
    /// of its `command_*` handlers, so this is the command path with the cell as
    /// its own sender — the BLE manager task cannot read the SRI itself, which is
    /// why the name is resolved here.
    #[cfg(feature = "ble")]
    pub fn handle_ble_callback(&self, export_name: &str, payload: Vec<u8>) -> Result<(), Error> {
        let command =
            Command::new(export_name.to_owned()).map_err(|err| Error::GuestFunctionException {
                function: export_name.to_owned(),
                exception: format!("unusable ble callback name: {err}"),
            })?;
        let sender = send_request_and_wait(CellHost::GetSri);

        self.handle_command(&command, Some(payload), Some(sender), CommandOrigin::Local)
    }

    /// Handles a timer tick by calling the target command handler.
    pub fn handle_timer_tick(&self, export_name: &str) -> Result<(), Error> {
        // The export name is used to actually call the command, so needs to be expanded first
        let expanded_name = format!("command_{export_name}");
        let args = Self::handler_args(None, Some(send_request_and_wait(CellHost::GetSri)));

        self.call_in_transaction(&expanded_name, &args, None)
            .map_err(|exception| Error::GuestFunctionException {
                function: export_name.to_owned(),
                exception,
            })
    }

    /// Handles a Cell [`Event`] by processing it and propagating it to the loaded WASM module
    pub fn handle_event(
        &self,
        event: &Event,
        payload: Vec<u8>,
        sender: Option<Sri>,
    ) -> Result<(), Error> {
        let expanded_name = format!("event_{}", event.as_ref());
        let args = Self::handler_args(Some(payload), sender);

        let event_error = |err_msg: String| Error::Event {
            event: event.as_ref().to_owned(),
            err_msg,
        };

        self.call_in_transaction(&expanded_name, &args, None)
            .map_err(event_error)
    }

    /// Runs a cell function as one application: invokes `export` (whose db host
    /// calls buffer into it, or flush it when they need a value back), removes
    /// `mailbox_entry` (the message that delivered the call, when there was
    /// one) in the same application now that the work is done, then closes it —
    /// committed when everything succeeded, rolled back when it did not, so a
    /// failed call leaves nothing behind and its command stays queued.
    ///
    /// Opening costs nothing: the transaction is placed by the first flush, so
    /// a call that never touches the db still ends in no round trips at all,
    /// and one that only writes ends in exactly one.
    fn call_in_transaction(
        &self,
        export: &str,
        args: &Vec<WasmValue>,
        mailbox_entry: Option<&Id>,
    ) -> Result<(), String> {
        open_application(scope_of_cell(send_request_and_wait(CellHost::GetSri)));

        let mut result = self.run_export(export, args);

        if let (Ok(()), Some(msg_id)) = (&result, mailbox_entry) {
            // Fails the call rather than committing without it: a turn that
            // cannot record the command as consumed must not look successful,
            // or the command is gone with its work unrecorded.
            result = consume_mailbox_entry(msg_id);
        }

        let Err(err) = result else {
            return commit_application()
                .map_err(|err| format!("failed to commit the transaction of '{export}': {err}"));
        };

        rollback_application();

        Err(err)
    }

    /// Invokes `export` and turns its exit code into the message the cell stored,
    /// treating any non-zero code as a failure whether or not it stored one.
    fn run_export(&self, export: &str, args: &Vec<WasmValue>) -> Result<(), String> {
        let values = match self.call_function(export, args) {
            Ok(values) => values,
            Err(err) => return Err(err.to_string()),
        };

        match values.first() {
            Some(WasmValue::I32(0)) if values.len() == 1 => Ok(()),
            Some(WasmValue::I32(_)) if values.len() == 1 => {
                Err(send_request_and_wait(CellHost::GetErrorMessage)
                    .unwrap_or_else(|| "Module error; No err msg stored".to_owned()))
            }
            _ => Err(format!("'{export}' did not return a valid status code")),
        }
    }

    /// Builds the argument vector for a command/event handler export and stores its payload.
    ///
    /// The myrmic-sdk handler macro exports `command_*` / `event_*` with the ABI
    /// `(id_hi, id_lo, sender_hi, sender_lo, arg_size) -> i32`.
    /// `id` is the cell's own SRI;
    /// `sender` is the sender of the command/event;
    /// `arg_size` is the payload length (the guest fetches the bytes via the argument buffer we
    /// store here).
    fn handler_args(payload: Option<Vec<u8>>, sender: Option<Sri>) -> Vec<WasmValue> {
        let payload = payload.unwrap_or_default();
        #[expect(
            clippy::expect_used,
            reason = "cell arguments are far smaller than i32::MAX"
        )]
        let arg_size = i32::try_from(payload.len()).expect("cell arguments should fit in an i32");
        send_request_and_wait(CellHost::StoreArguments(payload));

        let (id_hi, id_lo) = send_request_and_wait(CellHost::GetSri).as_parts();
        let (sender_hi, sender_lo) = sender.unwrap_or(Sri::NIL).as_parts();

        vec![
            WasmValue::I64(id_hi),
            WasmValue::I64(id_lo),
            WasmValue::I64(sender_hi),
            WasmValue::I64(sender_lo),
            WasmValue::I32(arg_size),
        ]
    }
}

impl WamrRuntime<'_> {
    fn call_function(&self, name: &str, args: &Vec<WasmValue>) -> Result<Vec<WasmValue>, Error> {
        log::trace!("about to call '{name}' function");
        let f = Function::find_export_func(&self.instance, name).map_err(Error::GuestFunction)?;
        self.call_wasm_function(&f, args)
    }

    fn call_wasm_function(
        &self,
        f: &Function<'_>,
        args: &Vec<WasmValue>,
    ) -> Result<Vec<WasmValue>, Error> {
        f.call(&self.instance, args).map_err(Error::GuestFunction)
    }
}

/// Calls a wasm exported function if one is found
fn call_if_present_on(
    instance: &Instance<'static>,
    name: &str,
    args: &Vec<WasmValue>,
) -> Result<Vec<WasmValue>, Error> {
    match Function::find_export_func(instance, name) {
        Ok(f) => {
            log::info!("about to call '{name}' function");
            f.call(instance, args).map_err(Error::GuestFunction)
        }
        Err(RuntimeError::FunctionNotFound) => {
            log::info!("Running WASM module without '{name}'.");
            Ok(vec![])
        }
        Err(e) => Err(Error::GuestFunction(e)),
    }
}

/// Same as `call_if_present_on` but for functions that might report their error through the cell's
/// error message host functions
fn call_if_present_cell_fn_on(
    instance: &Instance<'static>,
    name: &str,
    args: &Vec<WasmValue>,
) -> Result<Vec<WasmValue>, Error> {
    let ret_val = call_if_present_on(instance, name, args)?;

    // Double-check if there's an error message. The host function can still succeed but return an error
    if let Some(err_msg) = send_request_and_wait(CellHost::GetErrorMessage) {
        Err(Error::CellDeployment(err_msg))
    } else {
        Ok(ret_val)
    }
}

/// Opens the application a cell function will run in, routed to the cell's own
/// slice. Costs nothing until something is applied.
///
/// A guest handler can touch several scopes in one transaction — its private
/// data, a public namespace, its mailbox, another cell's inbox — and the scope
/// of each is a guest argument decoded only when that call arrives, so no exact
/// scope is knowable here. `Routed` is a placement hint rather than a boundary,
/// and the cell's own slice is where most of that traffic lands.
fn open_application(scope: Scope) {
    send_request_and_wait(DbClient::Open(scope));
}

/// Applies whatever the function left deferred and commits. Free when it
/// deferred nothing and never opened a transaction.
fn commit_application() -> Result<(), String> {
    send_request_and_wait(DbClient::Commit).map_err(|err| format!("{err}"))
}

/// Abandons the application. Costs nothing when nothing was ever flushed.
fn rollback_application() {
    send_request_and_wait(DbClient::Rollback);
}

/// Checks if this SRI has already a registry entry.
///
/// Read in a transaction of its own, routed to the registry's own scope, and so
/// answered by the highest-head holder of `sorg`. Reading it through the
/// deploy's application would place it on a holder of the *cell's* scope —
/// possibly a fallback-minted sink whose `sorg` replica is behind — and a stale
/// "not registered" re-runs `init_cell` on a cell whose init has already run.
fn is_sri_already_registered(sri: &Sri) -> Result<bool, Error> {
    let tb_get::Response { value } = send_request_and_wait(DbClient::ReadIn {
        scope: instance_registry_scope(),
        op: tb_get::Op {
            scope: instance_registry_scope(),
            table: INSTANCE_REGISTRY_TABLE.to_owned(),
            eid: sri.to_string().into_bytes(),
        }
        .into(),
    })
    .map_err(|err| Error::CellRegistration(format!("{err}")))?
    .try_into()
    .map_err(|_| Error::CellRegistration("unexpected registry response".to_owned()))?;

    Ok(value.is_some())
}

/// Removes the mailbox message that delivered a command, so its consumption
/// commits with the work it triggered.
///
/// Refused only when the transaction is already gone, in which case nothing
/// else the handler did is going to commit either — the command stays in the
/// mailbox and comes round again, which is the at-least-once contract.
fn consume_mailbox_entry(msg_id: &Id) -> Result<(), String> {
    let sri = send_request_and_wait(CellHost::GetSri);

    send_request_and_wait(DbClient::Defer(
        tb_delete::Op {
            scope: scope_of_cell(sri),
            table: MESSAGES_TABLE.to_owned(),
            eid: msg_id.clone(),
        }
        .into(),
    ))
    .map_err(|err| format!("unable to consume the mailbox entry: {err}"))
}

/// What a deploy needs to write about the cell it is bringing up.
#[derive(Clone, Copy)]
struct InitTarget<'a> {
    sri: &'a Sri,
    class_name: &'a str,
    gen_id: Gen,
    lineage: &'a SpawnLineage,
}

/// Calls the cell's `init_cell` export — skipped for an SRI that already has a
/// registry row, since its init has run before — and writes the instance row.
/// Both join the deploy's application.
#[expect(
    clippy::expect_used,
    reason = "cell arguments are far smaller than i32::MAX"
)]
fn run_cell_init(
    instance: &Instance<'static>,
    target: InitTarget<'_>,
    payload: Option<Vec<u8>>,
    already_registered: bool,
) -> Result<(), Error> {
    let InitTarget {
        sri,
        class_name,
        gen_id,
        lineage,
    } = target;

    if !already_registered {
        // `init_cell` matches the myrmic-sdk `#[init]` ABI `(id_hi, id_lo, sender_hi, sender_lo)`
        let (id_hi, id_lo) = sri.as_parts();
        let (sender_hi, sender_lo) = lineage.parent.unwrap_or(Sri::NIL).as_parts();
        // The argument slot outlives the cell it belongs to, so on a redeploy it still
        // holds whatever the previous life left behind. Overwrite it unconditionally,
        // as the command path does, so `init` only ever sees this deploy's arguments.
        let payload = payload.unwrap_or_default();
        let arg_size = payload.len();
        send_request_and_wait(CellHost::StoreArguments(payload));

        call_if_present_cell_fn_on(
            instance,
            "init_cell",
            &vec![
                WasmValue::I64(id_hi),
                WasmValue::I64(id_lo),
                WasmValue::I64(sender_hi),
                WasmValue::I64(sender_lo),
                WasmValue::I32(
                    i32::try_from(arg_size).expect("cell arguments should fit in an i32"),
                ),
            ],
        )?;
    }

    register_sri(sri, class_name, gen_id, lineage)
}

/// Registers the instance in the instance registry: one *life* of an SRI,
/// carrying the generation and the spawn edge that supervision reads.
fn register_sri(
    sri: &Sri,
    class_name: &str,
    gen_id: Gen,
    lineage: &SpawnLineage,
) -> Result<(), Error> {
    let value = postcard::to_allocvec(&CellInstance {
        sri: *sri,
        class_name: class_name.to_owned(),
        gen_id,
        lineage: lineage.clone(),
    })
    .map_err(|_| Error::CellRegistration("failed to serialize instance entry".to_owned()))?;

    send_request_and_wait(DbClient::Defer(
        tb_append::Op {
            scope: instance_registry_scope(),
            table: INSTANCE_REGISTRY_TABLE.to_owned(),
            eid: Some(sri.to_string().into_bytes()),
            value,
        }
        .into(),
    ))
    .map_err(|err| Error::CellRegistration(format!("unable to record the instance: {err}")))
}

/// WAMR log output callback — called by `esp_hal_platform.c` for all WAMR
/// diagnostic output. Routes through the standard `log` facade.
///
/// # Safety
/// `buf` must point to `len` valid UTF-8 (or ASCII) bytes.
///
/// # Panics
/// Will panic if `len` cannot fit in an i32
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wamr_esp_hal_write(buf: *const u8, len: u32) -> i32 {
    if buf.is_null() {
        return 0;
    }
    // safety: We just checked buf to be non-null
    let slice = unsafe { core::slice::from_raw_parts(buf, len as usize) };
    // Trim trailing newline/CR so log! doesn't double-newline
    let s = core::str::from_utf8(slice)
        .unwrap_or("<invalid utf8>")
        .trim_end_matches(['\n', '\r']);
    if !s.is_empty() {
        log::info!("[wamr] {s}");
    }
    #[expect(
        clippy::expect_used,
        reason = "We need to know if WAMR is breaking the contract"
    )]
    len.try_into().expect("len too large to fit in i32")
}

/// Shutdown hook that checks whether we should abort the execution of the WASM module during a
/// host function call.
///
/// # Returns
///
/// * `true`: if the module has to shutdown
/// * `false`: if the module can continue execution
fn shutdown(exec_env: wasm_exec_env_t) -> bool {
    if TERMINATE.load(Ordering::Acquire) {
        log::trace!("Shutting down WASM module");

        // safety: C FFI
        let module_inst = unsafe { sys::wasm_runtime_get_module_inst(exec_env) };
        if !module_inst.is_null() {
            // safety: C FFI
            unsafe {
                sys::wasm_runtime_set_exception(module_inst, c"SHUTDOWN".as_ptr());
            }
        }

        true
    } else {
        false
    }
}
