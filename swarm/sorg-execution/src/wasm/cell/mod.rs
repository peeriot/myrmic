//! Module for wasm-based cells

use std::collections::HashSet;
use std::time::Duration;

pub(crate) mod cell_task;
mod observability;
pub(crate) mod state;

use cell_protocol::{Gen, Sri};
use db_client::Session;
use opentelemetry::trace::SpanContext;
use sorg_common::{PoisonSnd, SpawnLineage, bail, custom_err};
use tokio::sync::oneshot::{Sender, channel};
use uuid::Uuid;
use wasmtime::{Engine, Instance, Module, Store};

/// Splits a cell identity UUID into the `(hi, lo)` `i64` pair passed across the
/// Wasm ABI. The guest recombines them into a `u128`. A `None` sender becomes
/// the nil UUID.
#[allow(clippy::cast_possible_wrap)] // reinterpret the bit pattern as i64
pub(crate) fn sri_parts(id: Option<Uuid>) -> (i64, i64) {
    let (hi, lo) = id.unwrap_or(Uuid::nil()).as_u64_pair();
    (hi as i64, lo as i64)
}

use crate::{
    Result,
    wasm::{
        WasmEnvironment,
        cell::{cell_task::CellRuntime, state::CellState},
        module_load::{load_module_cell, load_wasm_binary_from_class},
    },
};
use myrmic_common::cells::{Command, Event};

/// Message as it arrives from the channel — includes cross-cutting context
/// (e.g., span context) that `CellState::begin_message` consumes before
/// dispatch.
pub(crate) struct IncomingMessage {
    pub(crate) span_context: Option<SpanContext>,
    pub(crate) message: CellMessage,
    /// When the producer handed this message to the cell's channel, so the
    /// loop can record how long the hop took (`cell_recv_lag_nanos`) — the
    /// cell task's run-queue wait, which no other counter sees. `None` from
    /// producers that aren't instrumented (events, timers).
    pub(crate) queued_at: Option<std::time::Instant>,
}

impl IncomingMessage {
    pub(crate) fn ty(&self) -> &'static str {
        self.message.ty()
    }

    pub(crate) fn identifier(&self) -> &str {
        self.message.identifier()
    }
}

/// Message as seen by the cell dispatch logic — pure cell concerns, no
/// observability data.
pub(crate) enum CellMessage {
    Command(CellCommand),
    Event(CellEvent),
    TimerTick(CellTimerTick),
    TimerFinished(CellTimerFinished),
}

impl CellMessage {
    pub(crate) fn ty(&self) -> &'static str {
        match self {
            CellMessage::Command(_) => "command",
            CellMessage::Event(_) => "event",
            CellMessage::TimerTick(_) => "timer_tick",
            CellMessage::TimerFinished(_) => "timer_finished",
        }
    }

    pub(crate) fn identifier(&self) -> &str {
        match self {
            CellMessage::Command(cmd) => cmd.cmd.as_ref(),
            CellMessage::Event(cmd) => cmd.event.as_ref(),
            CellMessage::TimerTick(cmd) => &cmd.export_name,
            CellMessage::TimerFinished(_) => "timer_finished",
        }
    }
}

/// A call to one of the cell's `command_*` handlers. Most arrive through the
/// mailbox, but anything the host invokes by handler name — a BLE result
/// landing on the callback the cell registered — is delivered the same way.
pub(crate) struct CellCommand {
    pub cmd: Command,
    pub payload: Option<Vec<u8>>,
    pub origin: CommandOrigin,
    pub ready: Option<tokio::sync::oneshot::Sender<()>>,
    /// Identity of the cell that issued this command, if it came from one.
    pub sender: Option<Uuid>,
}

/// Where a command came from, and so what consuming it means.
pub(crate) enum CommandOrigin {
    /// Read from the cell's mailbox, and still in it. Removed inside the
    /// handler's transaction once the handler succeeds, so a command whose
    /// handling failed is left queued and delivered again.
    Mailbox(cell_mailbox::CommandReceipt),
    /// Raised by the host for the cell itself — a BLE result landing on the
    /// callback it registered. Nothing to consume.
    // The BLE backend is the only local raiser today, so without it nothing
    // constructs this.
    #[cfg_attr(not(feature = "ble-linux"), allow(dead_code))]
    Local,
}

pub(crate) struct CellEvent {
    pub event: Event,
    pub payload: Vec<u8>,
    /// Identity of the cell that published this event, if it came from one.
    pub sender: Option<Uuid>,
}

pub(crate) struct CellTimerTick {
    pub timer_id: u32,
    pub export_name: String,
    pub completed: Option<Sender<()>>,
}

pub(crate) struct CellTimerFinished {
    pub timer_id: u32,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_cell(
    wasm_env: &WasmEnvironment,
    session: &Session,
    sri: Sri,
    class_name: &str,
    gen_id: Gen,
    lineage: SpawnLineage,
    arguments: Option<Vec<u8>>,
    mailbox_poll_interval: Duration,
    mailbox_batch_size: usize,
) -> Result<(PoisonSnd, tokio::task::JoinHandle<Result<()>>)> {
    let event_handlers =
        event_handlers_from_cell_binary(&wasm_env.engine, session, class_name).await?;

    let (mut cell_state, msg_rcv, ready_rcv) = CellState::state_and_msg_rcv(
        sri,
        gen_id,
        lineage.clone(),
        session.clone(),
        event_handlers.clone(),
        mailbox_poll_interval,
        mailbox_batch_size,
    );

    // Every event handler the runtime discovered in the binary is subscribed
    // automatically — cells no longer register their own subscriptions.
    for name in event_handlers {
        let event = myrmic_common::cells::Event::new(name)
            .map_err(|e| sorg_common::custom_err!("invalid discovered event handler: {e}"))?;
        cell_state.subscribe_event(event)?;
    }

    let mut store = Store::new(&wasm_env.engine, cell_state);
    store.set_fuel(wasm_env.runner_fuel)?;
    store.fuel_async_yield_interval(Some(wasm_env.fuel_yield_interval))?;

    let instance = load_module_cell(
        session,
        class_name,
        &wasm_env.engine,
        &mut store,
        &wasm_env.linker,
    )
    .await?;

    call_cell_inits(
        &instance, &mut store, session, &sri, class_name, gen_id, &lineage, arguments,
    )
    .await?;

    let (poison_snd, poison_rcv) = channel();
    let handle = tokio::spawn(
        CellRuntime::new(instance, store, msg_rcv, sri, wasm_env.runner_fuel).run(poison_rcv),
    );

    // Wait until the command queryable is live before reporting the cell as loaded.
    if ready_rcv.await.is_err() {
        bail!("cell command listener failed to become ready");
    }

    Ok((poison_snd, handle))
}

/// Whether this incarnation still owes its `#[init]`: no registry row, or a
/// corpse row from an older incarnation whose erase hasn't replicated here
/// yet (the init tx's entry write supersedes it — restarts mint strictly
/// greater generations). A row at or past this deploy's generation is
/// authoritative: same gen means init already ran, newer means this body was
/// superseded mid-load and must not clobber its successor's row.
fn init_pending(existing: Option<Gen>, gen_id: Gen) -> bool {
    existing.is_none_or(|g| g < gen_id)
}

#[allow(clippy::too_many_arguments)]
async fn call_cell_inits(
    instance: &Instance,
    store: &mut Store<CellState>,
    session: &Session,
    sri: &Sri,
    class_name: &str,
    gen_id: Gen,
    lineage: &SpawnLineage,
    arguments: Option<Vec<u8>>,
) -> Result<()> {
    // init allocator, if function defined
    if let Ok(init_alloc_func) = instance.get_typed_func::<(), ()>(&mut *store, "init_allocator") {
        init_alloc_func.call_async(&mut *store, ()).await?;
    }

    let existing = sorg_common::instance_registry::get_instance(session, sri)
        .await?
        .map(|record| record.gen_id);
    if !init_pending(existing, gen_id) {
        return Ok(());
    }

    // Unlike other cell functions, init always has db work — the instance
    // registry entry, written right after it — so its transaction is opened up
    // front rather than lazily. The entry — the spawn lineage and incarnation
    // alongside the class — joins whatever state init wrote, so the cell only
    // goes on record together with it.
    let tx_id = store.data_mut().transaction().await?;

    let record = cell_protocol::CellInstance {
        sri: *sri,
        class_name: class_name.to_owned(),
        gen_id,
        lineage: lineage.clone(),
    };

    let result: Result<()> = async {
        call_init(instance, store, lineage, arguments).await?;
        sorg_common::instance_registry::insert_registry_entry_in_tx(session, tx_id, &record)
            .await?;
        Ok(())
    }
    .await;

    let application = store.data_mut().take_application();

    if let Err(err) = result {
        if let Some(application) = application {
            drop(application.rollback().await);
        }
        return Err(err);
    }

    if let Some(application) = application {
        application
            .commit()
            .await
            .map_err(|err| custom_err!("unable to commit cell init: {err}"))?;
    }

    Ok(())
}

/// Calls the cell's `init_cell` export, if it has one.
async fn call_init(
    instance: &Instance,
    store: &mut Store<CellState>,
    lineage: &SpawnLineage,
    arguments: Option<Vec<u8>>,
) -> Result<()> {
    let Ok(init_func) =
        instance.get_typed_func::<(i64, i64, i64, i64, i32), i32>(&mut *store, "init_cell")
    else {
        return Ok(());
    };

    // init receives the cell's own identity, and — for a spawned cell — the
    // spawning parent's identity as the sender (nil for root cells).
    let (id_hi, id_lo) = sri_parts(Some(store.data().sri().as_uuid()));
    let (sender_hi, sender_lo) = sri_parts(lineage.parent.map(|p| p.as_uuid()));

    // Deliver the spawn payload (if any) as init's argument buffer, exactly like
    // a command payload. A Void init rejects a non-empty buffer; a
    // payload-taking init decodes it.
    let payload = arguments.unwrap_or_default();
    let arg_size: i32 = payload
        .len()
        .try_into()
        .map_err(|_| custom_err!("init arguments too large"))?;
    store.data_mut().store_arguments(payload)?;

    let result = init_func
        .call_async(&mut *store, (id_hi, id_lo, sender_hi, sender_lo, arg_size))
        .await;

    // Clear in case a Void init short-circuited without consuming them.
    store.data_mut().clear_arguments();

    match result? {
        0 => Ok(()),
        err_code => {
            let err_msg = store
                .data_mut()
                .take_err_msg()
                .unwrap_or_else(|| format!("cell init failed with error code {err_code}"));
            bail!("{err_msg}")
        }
    }
}

pub(crate) async fn event_handlers_from_cell_binary(
    engine: &Engine,
    session: &Session,
    class_name: &str,
) -> Result<HashSet<String>> {
    let binary = load_wasm_binary_from_class(session, class_name).await?;
    let module = Module::from_binary(engine, &binary)?;

    let mut event_handlers = HashSet::new();

    for export in module.exports() {
        let name = export.name();
        if let Some(event_name) = name.strip_prefix("event_") {
            event_handlers.insert(event_name.to_owned());
        }
    }

    Ok(event_handlers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(time: u64) -> Gen {
        Gen::from_parts(time, 1)
    }

    #[test]
    fn init_runs_for_an_unregistered_sri() {
        assert!(init_pending(None, g(2)));
    }

    #[test]
    fn stale_corpse_row_does_not_suppress_init() {
        assert!(init_pending(Some(g(1)), g(2)));
    }

    #[test]
    fn own_registration_skips_init() {
        assert!(!init_pending(Some(g(2)), g(2)));
    }

    #[test]
    fn newer_incarnations_row_is_left_alone() {
        assert!(!init_pending(Some(g(3)), g(2)));
    }
}
