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
use tracing::{Instrument, debug, error};
use zenoh::{Session, query::Query};

use crate::{
    Config, Result,
    state::{State, StateUpdate},
    supervision::SharedLeaseTracker,
};

pub(crate) use events::Event;

mod cells;
mod events;

type EventReceiver = tokio::sync::mpsc::Receiver<Event>;

const EVENT_BUFFER_SIZE: usize = 10; // TODO what is a reasonable size for this one?

pub(crate) fn set_up_event_loop(
    session: Session,
    state: State,
    config: Config,
    lease_tracker: SharedLeaseTracker,
    poison_rcv: PoisonRcv,
) -> (Client<Event>, JoinHandle<Result<()>>) {
    let (event_sender, event_receiver) = tokio::sync::mpsc::channel(EVENT_BUFFER_SIZE);
    let client = Client::new(event_sender);
    let join_handle = tokio::spawn(
        event_loop(
            session,
            client.handle(),
            config,
            state,
            lease_tracker,
            event_receiver,
            poison_rcv,
        )
        .in_current_span(),
    );
    (client, join_handle)
}

async fn event_loop(
    session: Session,
    client: Client<Event>,
    config: Config,
    state: State,
    lease_tracker: SharedLeaseTracker,
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

        // Applied inline so that a leave and a re-join of one orch take effect in arrival order
        match event {
            Event::OrchJoining(zid) => {
                state.lock().await.add_orch(zid);
                continue;
            }
            Event::NodeLeaving(zid) => {
                state
                    .lock()
                    .await
                    .update_state(&StateUpdate::OrchLeave(zid));
                continue;
            }
            Event::InfoQuery(..)
            | Event::ProcessError(..)
            | Event::CellDeployQuery(..)
            | Event::CellUndeployQuery(..)
            | Event::AppDeleteQuery(..) => {}
        }

        // All orchs react to info query, leader processes all events:
        if is_event_relevant(&state, &event).await? {
            let runtime = Runtime::new(
                session.clone(),
                client.handle(),
                config.init_timeout(),
                lease_tracker.clone(),
            );
            tokio::spawn(runtime.process_event(event).in_current_span());
        }
    }
    Ok(())
}

async fn is_event_relevant(state: &State, event: &Event) -> Result<bool> {
    match event {
        // Always reacting to these
        Event::InfoQuery(..) | Event::ProcessError(..) => Ok(true),
        // Already applied inline by the event loop
        Event::NodeLeaving(..) | Event::OrchJoining(..) => Ok(false),
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
    lease_tracker: SharedLeaseTracker,
}

impl Runtime {
    fn new(
        session: Session,
        client: Client<Event>,
        init_timeout: Duration,
        lease_tracker: SharedLeaseTracker,
    ) -> Self {
        Self {
            session,
            client,
            init_timeout,
            lease_tracker,
        }
    }

    async fn process_event(self, event: Event) {
        let result = match event {
            Event::InfoQuery(query) => self.provide_info(query).await,
            Event::ProcessError(_) => unreachable!("process error handled by event loop itself"),
            Event::NodeLeaving(_) | Event::OrchJoining(_) => {
                unreachable!("membership events handled by event loop itself")
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
}

#[cfg(test)]
mod tests {
    use std::{
        str::FromStr,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use sorg_common::{poison_channel, supervision::LeaseTracker};
    use sorg_tests::test_session;
    use tokio::time::timeout;
    use zenoh::config::ZenohId;

    use super::{Event, set_up_event_loop};
    use crate::{Config, init_state};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn orch_leave_then_rejoin_stays_known() {
        let session = test_session().await;
        let state = init_state(&session);
        let (_poison_snd, poison_rcv) = poison_channel();
        let (client, _handle) = set_up_event_loop(
            session.clone(),
            state.clone(),
            Config::default(),
            Arc::new(Mutex::new(LeaseTracker::new())),
            poison_rcv,
        );
        let orch = ZenohId::from_str("37c72f467bc9c77f41b73fe16f054741").expect("valid zid");
        let sentinel = ZenohId::from_str("48d83a578cda88a052c84af27f165852").expect("valid zid");

        client
            .send(Event::OrchJoining(orch))
            .await
            .expect("event loop is running");
        client
            .send(Event::NodeLeaving(orch))
            .await
            .expect("event loop is running");
        client
            .send(Event::OrchJoining(orch))
            .await
            .expect("event loop is running");
        client
            .send(Event::OrchJoining(sentinel))
            .await
            .expect("event loop is running");
        timeout(Duration::from_secs(5), async {
            while !state.lock().await.knows_orch(sentinel) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sentinel join was not applied");

        assert!(state.lock().await.knows_orch(orch));
    }
}
