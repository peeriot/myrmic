use std::sync::Arc;
use std::time::Duration;

use sorg_common::{PoisonRcv, SorgPayload, bail, custom_err, poison_channel, set_up_queryable};
use tracing::{Level, debug, info, span, warn};
use zenoh::Session;

use crate::{
    Config, Result,
    event_loop::set_up_event_loop,
    init_state,
    membership::monitor_orch_nodes,
    queryables::Queryable,
    state::{State, StateUpdate},
    topics::TOPIC_ORCH_STATE_UPDATE,
};

/// Spawns a tokio task running the sorg orchestration. The task can be terminated using
/// the sender part of the one-shot channel which is provided to this method
pub async fn spawn(
    session: Session,
    config: Config,
    off_rcv: flume::Receiver<()>,
    ready: Arc<tokio::sync::Notify>,
) -> Result<()> {
    info!("spawning sorg-orchestration");

    // the poison senders are not actively used, but will poison the other end when we leave the scope
    let (_poison_snd_event_loop, poison_rcv_event_loop) = poison_channel();

    let node_id = session.info().zid().await;
    let node_span = span!(Level::INFO, "orch", node_id = %node_id);
    let _enter = node_span.enter();

    let orch_state = init_state(&session);

    // set up a task listening for updates of the orch deployment state
    let (_poison_snd_state_update, poison_rcv_state_update) = poison_channel();
    let state_update_handle = {
        let session = session.clone();
        let orch_state = orch_state.clone();
        tokio::task::spawn(process_deployment_updates(
            poison_rcv_state_update,
            orch_state,
            session,
        ))
    };

    let _lease_watcher = crate::supervision::spawn_lease_watcher(
        session.clone(),
        orch_state.clone(),
        sorg_common::supervision::SupervisionTiming::default(),
    );

    let (client, handle_event_loop) =
        set_up_event_loop(session.clone(), orch_state, config, poison_rcv_event_loop);

    // set up the handler for the leaving and joining of other nodes
    monitor_orch_nodes(session.clone(), client.handle()).await?;

    let (handle_capas, _poison_snd_capas) =
        set_up_queryable(session.clone(), client.handle(), Queryable::Capabilities);
    let (handle_cell_deploy, _poison_snd_cell_deploy) =
        set_up_queryable(session.clone(), client.handle(), Queryable::CellDeploy);
    let (handle_cell_undeploy, _poison_snd_cell_undeploy) =
        set_up_queryable(session.clone(), client.handle(), Queryable::CellUndeploy);
    let (handle_app_delete, _poison_snd_app_delete) =
        set_up_queryable(session.clone(), client.handle(), Queryable::AppDelete);

    // wait a bit for evth to set up before signaling readiness
    tokio::time::sleep(Duration::from_millis(100)).await;
    ready.notify_one();
    info!("orch plugin ready");

    tokio::select! {
        orch_result = handle_event_loop => {
            let orch_result = match orch_result{
                Ok(res) => res,
                Err(join_err) => {
                    bail!("the orch. event loop has panicked or was canceled: {join_err}");
                }
            };
            match orch_result{
                Ok(()) => bail!("event loop terminated without error"),
                Err(err) => bail!("event loop terminated due to error: '{err}'")
            }
        }

        _ = handle_capas => {
            bail!("capas queryable terminated");
        }

        _ = handle_cell_deploy => {
            bail!("cell deploy handle terminated");
        }

        _ = handle_cell_undeploy => {
            bail!("cell undeploy handle terminated");
        }

        _ = handle_app_delete => {
            bail!("app delete handle terminated");
        }

        _ = state_update_handle => {
            tracing::error!("state updated task terminated");
            bail!("state update task terminated");
        }

        _ = off_rcv.recv_async() => {
            debug!("sorg orchestration received shutdown signal");
        }
    }
    debug!("shutting down sorg orchestration");
    Ok(())
}

async fn process_deployment_updates(
    mut poison_rcv: PoisonRcv,
    state: State,
    session: Session,
) -> Result<()> {
    let update_subscriber = session
        .declare_subscriber(TOPIC_ORCH_STATE_UPDATE)
        .await
        .map_err(|err| custom_err!("failed to declare orch state update subscriber: {err}"))?;
    debug!("declared orch state update subscriber on topic '{TOPIC_ORCH_STATE_UPDATE}'");
    let own_id = session.zid();
    loop {
        tokio::select! {
            // state update
            update_sample_result = update_subscriber.recv_async() => {
                let mut state_lock = state.lock().await;
                if state_lock.is_leader()?{ // leader updates itself, so it can ignore the update message
                    debug!("leader ({own_id}) ignoring update signal");
                }else{
                    debug!("follower processing update signal");
                    let update_sample = update_sample_result.map_err(|err| custom_err!("sample error orch state update: {err}"))?;
                    let state_update = StateUpdate::from_payload(update_sample.payload(), "deser orch state update")?;
                    state_lock.update_state(&state_update);
                }
            }

            // shutdown signal
            _ = &mut poison_rcv => {
                warn!("node {own_id} terminated update task");
                break;
            }
        }
    }
    warn!("orch state update subscriber terminated");
    Ok(())
}
