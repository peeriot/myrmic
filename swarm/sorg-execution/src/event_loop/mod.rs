//! Module containing the event loop of the execution plugin
//! The event loop is the main mechanism for maintaining state and controlling the behavior of the execution plugin.
//! It is built around polling a queue of control events and processing them by triggering the correct action
//! Other parts of the execution plugin interact with the event loop by sending it control event via the event loop
//! client handles.
//!
//! In the execution plugin, all information that is maintained about the deployments on the managed runtime is owned
//! by the event loop. Consequently, processing event should be either done as very lightweight tasks (which can directly)
//! alter the state of the event loop or be run as extra tasks (which cannot directly change the state, but can produce
//! events which are processed by the event loop).

use std::collections::HashMap;
use std::time::Duration;

use cell_protocol::Sri;
use std::borrow::ToOwned;

use sorg_common::{
    Client, ExecConfig, ExecRuntimeInfo, ExecutionCapabilities, PoisonRcv, SorgPayload, zenoh_err,
};

use crate::event_loop::cells::CellHandle;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};
use zenoh::{Session, query::Query};

use crate::{Result, wasm::WasmEnvironment};

mod cells;
mod events;
mod supervision_pass;

pub(crate) use events::Event;

type EventReceiver = tokio::sync::mpsc::Receiver<Event>;

pub(crate) fn set_up_event_loop(
    session: Session,
    config: ExecConfig,
    poison_rcv: PoisonRcv,
) -> (Client<Event>, JoinHandle<Result<()>>) {
    let (event_sender, event_receiver) = tokio::sync::mpsc::channel(config.event_buffer_size());
    let client = Client::new(event_sender);
    let join_handle = tokio::spawn(event_loop(
        session,
        config,
        client.handle(),
        event_receiver,
        poison_rcv,
    ));
    (client, join_handle)
}

async fn event_loop(
    session: Session,
    config: ExecConfig,
    events: Client<Event>,
    mut event_rcv: EventReceiver,
    mut poison_rcv: PoisonRcv,
) -> Result<()> {
    let mut runtime = Runtime::new(session, &config, events)?;
    let mut verify = tokio::time::interval(sorg_common::supervision::jittered(
        runtime.timing.verify,
        u64::from(std::process::id()),
    ));
    verify.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // Regular event
            event = event_rcv.recv() => {
                let Some(event) = event else{
                    error!("event channel closed");
                    break;
                };
                runtime.process_event(event).await?;
            }

            _ = verify.tick() => {
                runtime.process_event(Event::VerifyPass).await?;
            }

            // Shutdown
            _ = &mut poison_rcv => {
                debug!("shutting down exec event loop"); break;
            }

        }
    }
    Ok(())
}

/// Represents the state and the behavior of the execution-related functionality of the sorg runtime
struct Runtime {
    info: ExecRuntimeInfo,
    session: Session,
    wasm_environment: WasmEnvironment,
    mailbox_poll_interval: Duration,
    mailbox_batch_size: usize,
    cells: HashMap<Sri, CellHandle>,
    events: Client<Event>,
    timing: sorg_common::supervision::SupervisionTiming,
    /// Spawn lineage per hosted cell — the fencing pass's watch list.
    meta: HashMap<Sri, crate::supervision::fencing::WatchedCell>,
    fencing: crate::supervision::fencing::FencingState,
    lease_tracker: sorg_common::supervision::LeaseTracker,
    /// Registry cleanup owed for cells this exec killed; drained each
    /// verification pass, kept across db outages (spec §3 retry queue).
    cleanup: Vec<CleanupAction>,
    /// Whether the previous-incarnation sweep has completed cleanly.
    sweep_done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CleanupAction {
    ReleaseCell(Sri),
    EraseInstance(Sri),
}

impl Runtime {
    fn new(session: Session, config: &ExecConfig, events: Client<Event>) -> Result<Self> {
        let wasm_environment =
            WasmEnvironment::new(config.runner_fuel(), config.fuel_yield_interval(), None)?;
        let info = ExecRuntimeInfo::new(
            session.zid(),
            config.name().map(ToOwned::to_owned),
            ExecutionCapabilities::new(config.capability_tags().to_vec()),
        );
        let timing = sorg_common::supervision::SupervisionTiming::default();

        let runtime = Self {
            info,
            session,
            wasm_environment,
            mailbox_poll_interval: config.mailbox_poll_interval(),
            mailbox_batch_size: config.mailbox_batch_size(),
            cells: HashMap::new(),
            events,
            timing,
            meta: HashMap::new(),
            fencing: crate::supervision::fencing::FencingState::new(),
            lease_tracker: sorg_common::supervision::LeaseTracker::new(),
            cleanup: Vec::new(),
            sweep_done: false,
        };
        info!("exec created");
        Ok(runtime)
    }

    async fn process_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::InfoQuery(query) => self.provide_info(query).await,
            Event::CellDeployQuery(query) => self.deploy_cell(query).await,
            Event::CellUndeployQuery(query) => self.undeploy_cell(query).await,
            Event::CellExited(sri) => {
                self.handle_cell_exited(sri);
                Ok(())
            }
            Event::VerifyPass => {
                self.verify_pass().await;
                Ok(())
            }
        }
    }

    async fn provide_info(&self, query: Query) -> Result<()> {
        let response_payload = self.info.clone().to_payload()?;
        query
            .reply(query.key_expr().clone(), response_payload)
            .await
            .map_err(|zen_err| zenoh_err!("exec replying to capability query", zen_err))?;
        debug!("exec has provided info");
        Ok(())
    }
}
