//! Module containing the event loop of the orchestration plugin
//! The event loop is the main mechanism for controlling the behavior of the orchestration plugin.
//! It is built around polling a queue of control events and processing them by triggering the correct action
//! Other parts of the orchestration plugin interact with the event loop by sending it control event via the event loop
//! client handles.
//!
//! As of now, the orchestration plugin is completely stateless. All events are processed within extra tasks which are
//! spawned and any information required for the orchestraion is obtained by querying the swarm.

use std::time::Duration;

use sorg_common::{Client, OrchRuntimeRecord, PoisonRcv, SorgPayload, bail, zenoh_err};
use tokio::task::JoinHandle;
use tracing::{debug, error};
use zenoh::{Session, config::ZenohId, query::Query};

use crate::{Config, Result, state::State};

pub(crate) use events::Event;

mod cells;
mod events;
mod node_leaving;

type EventReceiver = tokio::sync::mpsc::Receiver<Event>;

const EVENT_BUFFER_SIZE: usize = 10; // TODO what is a reasonable size for this one?

pub(crate) fn set_up_event_loop(
    session: Session,
    state: State,
    config: Config,
    poison_rcv: PoisonRcv,
) -> (Client<Event>, JoinHandle<Result<()>>) {
    let (event_sender, event_receiver) = tokio::sync::mpsc::channel(EVENT_BUFFER_SIZE);
    let client = Client::new(event_sender);
    let join_handle = tokio::spawn(event_loop(
        session,
        client.handle(),
        config,
        state,
        event_receiver,
        poison_rcv,
    ));
    (client, join_handle)
}

async fn event_loop(
    session: Session,
    client: Client<Event>,
    config: Config,
    state: State,
    mut event_rcv: EventReceiver,
    mut poison_rcv: PoisonRcv,
) -> Result<()> {
    loop {
        let event = tokio::select! {
            // Regular event
            event = event_rcv.recv() => {
                let Some(event) = event else{
                    error!("event channel closed");
                    break;
                };
                event
            }

            // Shutdown
            _ = &mut poison_rcv => {
                debug!("shutting down exec event loop"); break;
            }
        };

        if let Event::ProcessError(error) = event {
            bail!("orchestration loop shut down due to event processing error: {error}");
        }

        // All orchs react to info query, leader processes all events:
        if is_event_relevant(&state, &event).await? {
            let runtime = Runtime::new(
                session.clone(),
                client.handle(),
                config.init_timeout(),
                state.clone(),
            );
            tokio::spawn(runtime.process_event(event));
        }
    }
    Ok(())
}

async fn is_event_relevant(state: &State, event: &Event) -> Result<bool> {
    match event {
        // Always reacting to these
        Event::InfoQuery(..)
        | Event::ProcessError(..)
        | Event::NodeLeaving(..)
        | Event::OrchJoining(..) => Ok(true),
        // Handled by the leader
        Event::CellDeployQuery(..) | Event::CellUndeployQuery(..) | Event::AppDeleteQuery(..) => {
            state.lock().await.is_leader()
        }
    }
}

struct Runtime {
    session: Session,
    client: Client<Event>,
    init_timeout: Duration,
    state: State,
}

impl Runtime {
    fn new(session: Session, client: Client<Event>, init_timeout: Duration, state: State) -> Self {
        Self {
            session,
            client,
            init_timeout,
            state,
        }
    }

    async fn process_event(self, event: Event) {
        let result = match event {
            Event::InfoQuery(query) => self.provide_info(query).await,
            Event::ProcessError(_) => unreachable!("process error handled by event loop itself"),
            Event::NodeLeaving(zenoh_id) => self.handle_leaving_node(zenoh_id).await,
            Event::OrchJoining(zenoh_id) => {
                self.handle_joining_orch(zenoh_id).await;
                Ok(())
            }
            Event::CellDeployQuery(query) => self.handle_deploy_cell_query(query).await,
            Event::CellUndeployQuery(query) => self.handle_undeploy_cell_query(query).await,
            Event::AppDeleteQuery(query) => self.delete_app(query).await,
        };

        match result {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!("encounted error while attempting to process event: {}", err);
                let send_result = self.client.send(Event::ProcessError(err)).await;
                if send_result.is_err() {
                    error!("failed to notify orch event loop of error");
                }
            }
        }
    }

    async fn provide_info(&self, query: Query) -> Result<()> {
        let id = self.session.zid().into();
        let orch_rcd = OrchRuntimeRecord { id };
        let payload = orch_rcd.to_payload()?;
        query
            .reply(query.key_expr().clone(), payload)
            .await
            .map_err(|zen_err| zenoh_err!("orch failed to reply to capas query", zen_err))?;
        Ok(())
    }

    async fn handle_joining_orch(&self, zid: ZenohId) {
        self.state.lock().await.add_orch(zid);
    }
}
