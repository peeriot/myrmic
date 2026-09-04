use std::{collections::HashMap, sync::Arc};

use anyhow::{Result, anyhow};
use introspection_common::v1::{
    NodeStatus, ParticipantInfo, node_id, topic_liveliness_all, topic_liveliness_own,
    topic_node_join, topic_node_leave,
};
use serde_json::Value;
use tokio::sync::{Mutex, oneshot::Receiver};
use tracing::debug;
use zenoh::{Session, config::ZenohId, liveliness::LivelinessToken, sample::SampleKind};

use super::run::get_node_status;

pub(crate) type CurrentNodes = Arc<Mutex<HashMap<ZenohId, NodeStatus>>>;

/// Sets up the liveliness tokens that the nodes use to monitor the nodes in the network
pub(super) async fn set_up_liveliness_token(session: &Session) -> Result<LivelinessToken> {
    let own_id = session.zid();
    let my_live_topic = topic_liveliness_own(own_id)?;
    let live_token = session
        .liveliness()
        .declare_token(&my_live_topic)
        .await
        .map_err(|err| anyhow!("failed to declare liveliness topic: {err}"))?;
    debug!("declared liveliness token on {my_live_topic}");
    Ok(live_token)
}

/// Subscribes to our own join topic to collect `NodeStatus` messages from other nodes.
/// Must be called BEFORE declaring the liveliness token so we don't miss any messages.
pub(super) async fn subscribe_to_joins(
    session: &Session,
    current_nodes: CurrentNodes,
) -> Result<()> {
    let our_id = session.zid();
    let join_topic = topic_node_join(our_id)?;
    let subscriber = session
        .declare_subscriber(&join_topic)
        .await
        .map_err(|err| anyhow!("failed to subscribe to own join topic: {err}"))?;
    debug!("subscribed to own join topic {join_topic}");

    tokio::task::spawn(async move {
        while let Ok(sample) = subscriber.recv_async().await {
            let bytes = sample.payload().to_bytes();
            match serde_json::from_slice::<NodeStatus>(&bytes) {
                Ok(node_status) => {
                    if node_status.id != our_id {
                        debug!("current_nodes: added {}", node_status.id);
                        current_nodes
                            .lock()
                            .await
                            .insert(node_status.id, node_status);
                    }
                }
                Err(err) => {
                    tracing::warn!("failed to deserialize join NodeStatus: {err}");
                }
            }
        }
    });

    Ok(())
}

/// Task to monitor the set of present orch runtimes and update the orch information in the state of this orch instance
pub(super) async fn monitor_nodes(
    mut poison_rcv: Receiver<()>,
    session: Session,
    current_nodes: CurrentNodes,
    plugins: Value,
    participant: Option<ParticipantInfo>,
) -> Result<()> {
    let live_topic = topic_liveliness_all()?;
    let subscriber = session
        .liveliness()
        .declare_subscriber(&live_topic)
        .history(true)
        .await
        .map_err(|err| anyhow!("failed to declare liveliness subscriber: {err}"))?;
    debug!("declared orch liveliness monitor on topic {live_topic}");

    loop {
        tokio::select! {
            // Liveliness update
            sample_result = subscriber.recv_async() => {
                let change = sample_result.map_err(|err| anyhow!("sample error liveliness update: {err}"))?;
                match change.kind(){
                    SampleKind::Put => {
                        let own_status = get_node_status(&session, &plugins, participant.as_ref()).await.map_err(|err_msg| anyhow!("error getting own status: {err_msg}"))?;
                        let id_new_node = node_id(change.key_expr())?;
                        debug!("node {own_id} registered join of node {other_id}", own_id = session.zid(), other_id = id_new_node);
                        let join_topic_new_node = topic_node_join(id_new_node)?;
                        let status_payload = serde_json::to_vec(&own_status)?;
                        session
                            .put(join_topic_new_node, status_payload)
                            .await
                            .map_err(|err| anyhow!("failed to announce self to joining node: {err}"))?;
                    }
                    SampleKind::Delete => {
                        let removed_id = node_id(change.key_expr())?;
                        debug!("node {own_id} registered leave of node {other_id}", own_id = session.zid(), other_id = removed_id);
                        current_nodes.lock().await.remove(&removed_id);
                        {
                            const DEREGISTER_MAX_RETRIES: u32 = 5;
                            const DEREGISTER_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);
                            let result = tryhard::retry_fn(|| {
                                sorg_common::exec_registry::deregister_exec(&session, removed_id)
                            })
                            .retries(DEREGISTER_MAX_RETRIES)
                            .fixed_backoff(DEREGISTER_BACKOFF)
                            .on_retry(|attempt, _, err: &_| {
                                let err = err.to_string();
                                async move {
                                    debug!("exec deregistration failed (attempt {attempt}/{DEREGISTER_MAX_RETRIES}): {err}");
                                }
                            })
                            .await;
                            if let Err(err) = result {
                                tracing::error!("failed to deregister leaving exec {removed_id} after {DEREGISTER_MAX_RETRIES} retries: {err}");
                            }
                        }
                        notify_plugins_leave(removed_id, &session).await?;
                    }
                }
            }

            // Shutdown signal
            _ = &mut poison_rcv => {
                break;
            }
        }
    }
    debug!("liveliness monitor terminated");
    Ok(())
}

async fn notify_plugins_leave(id_leaving: ZenohId, session: &Session) -> Result<()> {
    let id_own = session.info().zid().await;
    let notify_topic = topic_node_leave(id_own)?;
    let payload = serde_json::to_vec(&id_leaving)
        .map_err(|err| anyhow!("failed to serialize zenoh id: {err:?}"))?;
    session
        .put(notify_topic, payload)
        .await
        .map_err(|err| anyhow!("failed to notify plugins about leaving node: {err:?}"))?;

    Ok(())
}
