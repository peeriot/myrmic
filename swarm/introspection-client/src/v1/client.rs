use std::future::Future;

use anyhow::{Context, Result, anyhow, bail};

use introspection_common::v1::{
    NodeStatus, ParticipantInfo, TOPIC_NODE_STATUS, topic_current_nodes, topic_node_join,
    topic_node_leave,
};

use tracing::error;
use zenoh::{Session, config::ZenohId, query::ConsolidationMode};

pub struct Client {
    session: Session,
    id: ZenohId,
}

impl Client {
    #[must_use]
    pub async fn new(session: Session) -> Self {
        let id = session.info().zid().await;
        Self { session, id }
    }

    pub async fn swarm_status(&self) -> Result<Vec<NodeStatus>> {
        let mut node_statuses = self.query_node_statuses().await?;
        remove_connecting_node(&mut node_statuses, &self.id);
        Ok(node_statuses)
    }

    pub async fn own_status(&self) -> Result<NodeStatus> {
        // this is called from other plugins on the same node. Since it is possible that the intro
        // plugin spins up after the caller plugin, we poll in a loop here
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let node_statuses = self.query_node_statuses().await?;
                if let Some(own) = node_statuses.into_iter().find(|ns| ns.id == self.id) {
                    return Ok(own);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .context("timed out waiting for own node status from introspection plugin")?
    }

    /// Returns the `NodeStatus` of all nodes currently known to the introspection plugin
    /// (excluding ourselves).
    ///
    /// Unlike `on_join`, which only fires for future join events, this method queries the
    /// introspection plugin for the current set of known nodes. Use this to seed initial state,
    /// then use `on_join`/`on_leave` for ongoing changes.
    ///
    /// **Important**: Like `on_join`/`on_leave`, this method requires an introspection plugin
    /// running in the same zenoh session.
    pub async fn current_nodes(&self) -> Result<Vec<NodeStatus>> {
        let topic = topic_current_nodes(self.id)?;
        let replies = self
            .session
            .get(&topic)
            .await
            .map_err(|err| anyhow!("error querying current nodes: {err}"))?;

        let reply = replies
            .recv_async()
            .await
            .map_err(|err| anyhow!("no reply from current-nodes queryable: {err}"))?;

        let sample = reply.result().map_err(|err| {
            anyhow!(
                "current-nodes query error: {}",
                err.payload().try_to_string().unwrap_or_default()
            )
        })?;

        let nodes: Vec<NodeStatus> = serde_json::from_slice(&sample.payload().to_bytes())
            .context("deserializing current nodes")?;

        Ok(nodes)
    }

    async fn query_node_statuses(&self) -> Result<Vec<NodeStatus>> {
        let status_replies = self
            .session
            .get(TOPIC_NODE_STATUS)
            .consolidation(ConsolidationMode::None)
            .await
            .map_err(|err| anyhow!("error getting status replies: {err}"))?;

        let mut node_statuses = vec![];

        while let Ok(reply) = status_replies.recv_async().await {
            match reply.result() {
                Ok(payload) => {
                    let node_status = serde_json::from_slice(&payload.payload().to_bytes())
                        .context("deserializing node status")?;
                    node_statuses.push(node_status);
                }
                Err(err_payload) => {
                    let err_msg = err_payload
                        .payload()
                        .try_to_string()
                        .context("reading err reply")?;
                    bail!("failed getting swarm status: {err_msg}");
                }
            }
        }

        Ok(node_statuses)
    }

    /// Registers a callback function to be executed when a new node joins the swarm.
    ///
    /// This method sets up a subscription to node join events emitted by the introspection plugin
    /// and executes the provided callback whenever a new node (other than the current node) joins
    /// the swarm. The callback receives the `NodeStatus` information of the joining node.
    ///
    /// **Important**: Unlike most client APIs, this method requires an introspection plugin to be
    /// running in the same zenoh session. The method will not work correctly if no introspection
    /// plugin is present, as it relies on the plugin to publish node join events. As such, this
    /// method should be rather used from other plugins running on the same node as the
    /// introspection plugin then as a programming API.
    ///
    /// # Arguments
    ///
    /// * `state` - A shared state object that will be cloned (its cloning should be cheap) and passed to each callback invocation.
    ///   This allows the callback to access and modify shared state across multiple invocations.
    /// * `callback_fn` - A closure that takes a `NodeStatus` (the joining node's status) and
    ///   the shared state, and returns a future. This closure will be executed for each node join event.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if the callback was successfully registered, or an error if registration failed.
    ///
    pub async fn on_join<F, Fut, S>(&self, state: S, callback_fn: F) -> Result<()>
    where
        F: Fn(NodeStatus, S) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
        S: Send + Sync + 'static + Clone,
    {
        let session = self.session.clone();
        let topic_join_own = topic_node_join(session.info().zid().await)
            .context("failed to get own node join topic")?;

        let subscriber = session
            .declare_subscriber(topic_join_own)
            .await
            .map_err(|err| anyhow!("failed to subscribe to node join topic: {err}"))?;

        tokio::task::spawn(async move {
            while let Ok(sample) = subscriber.recv_async().await {
                let bytes = sample.payload().to_bytes();
                let node_status: NodeStatus =
                    serde_json::from_slice(&bytes).expect("failed to deser nodestatus");

                if node_status.id != session.info().zid().await {
                    callback_fn(node_status, state.clone()).await;
                }
            }

            error!("node join subscriber died for some reason");
        });

        Ok(())
    }

    /// Registers a callback function to be executed when a node leaves the swarm.
    ///
    /// This method sets up a subscription to node leave events and executes the provided callback
    /// whenever a node (other than the current node) leaves the swarm. The callback receives the
    /// `ZenohId` of the leaving node.
    ///
    /// **Important**: Unlike most client APIs, this method requires an introspection plugin to be
    /// running in the same zenoh session. The method will not work correctly if no introspection
    /// plugin is present, as it relies on the plugin to publish node leave events. As such, this
    /// method should be rather used from other plugins running on the same node as the
    /// introspection plugin then as a programming API.
    ///
    /// # Arguments
    ///
    /// * `state` - A shared state object that will be cloned (its cloning should be cheap) and passed to each callback invocation.
    ///   This allows the callback to access and modify shared state across multiple invocations.
    /// * `callback_fn` - A closure that takes a `ZenohId` (the leaving node's ID) and the shared
    ///   state, and returns a future. This closure will be executed for each node leave event.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if the callback was successfully registered, or an error if registration failed.
    pub async fn on_leave<F, Fut, S>(&self, state: S, callback_fn: F) -> Result<()>
    where
        F: Fn(ZenohId, S) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
        S: Send + Sync + 'static + Clone,
    {
        let session = self.session.clone();
        let topic_node_leave = topic_node_leave(session.info().zid().await)
            .context("failed to get own node leave topic")?;

        let subscriber = session
            .declare_subscriber(topic_node_leave)
            .await
            .map_err(|err| anyhow!("failed to subscribe to node leave topic: {err}"))?;

        tokio::task::spawn(async move {
            while let Ok(sample) = subscriber.recv_async().await {
                let bytes = sample.payload().to_bytes();
                let node_id: ZenohId =
                    serde_json::from_slice(&bytes).expect("failed to deser node id");

                if node_id != session.info().zid().await {
                    callback_fn(node_id, state.clone()).await;
                }
            }

            error!("node leave subscriber died for some reason");
        });

        Ok(())
    }
}

/// Answers node-status queries with a self-described [`NodeStatus`], so a
/// session that hosts no introspection plugin — a CLI invocation, say — shows
/// up labeled in network listings instead of as a bare id. Serves until the
/// session is closed.
///
/// Only for sessions *without* the introspection plugin. This queryable answers
/// on the same topic the plugin does, so on a plugin-hosting session the node
/// reports twice under one id, and the reply carrying `plugins: []` may be the
/// one a consumer keeps — enough for [`Client::own_status`] to satisfy a caller
/// waiting for a particular plugin to appear. A plugin-hosting node carries its
/// self-description in the plugin's `participant` config instead.
pub async fn declare_participant(session: &Session, info: ParticipantInfo) -> Result<()> {
    let queryable = session
        .declare_queryable(TOPIC_NODE_STATUS)
        .await
        .map_err(|err| anyhow!("failed to declare participant status queryable: {err}"))?;

    let session = session.clone();
    tokio::task::spawn(async move {
        while let Ok(query) = queryable.recv_async().await {
            let status = NodeStatus::of_session(&session, Some(info.clone()), vec![]).await;
            // Never `reply_err` here: consumers treat one error reply as a
            // failure of the whole status query.
            match serde_json::to_vec(&status) {
                Ok(payload) => {
                    if let Err(err) = query.reply(query.key_expr(), payload).await {
                        error!("participant failed to reply to status query: {err}");
                    }
                }
                Err(err) => error!("participant failed to serialize its status: {err}"),
            }
        }
    });

    Ok(())
}

// Removes the ID of the session we use to connect to the swarm, unless we share a session with sth hosting an
// introspection plugin. Our own bare self-description is likewise dropped: it is meant for other
// observers, not for our own listing. An empty plugin list is what tells the two apart — a node
// running the plugin always reports at least that plugin, a bare participant never reports any —
// so a plugin-hosting node keeps its row whether or not it also describes itself.
fn remove_connecting_node(node_statuses: &mut Vec<NodeStatus>, our_id: &ZenohId) {
    node_statuses.retain(|ns| ns.id != *our_id || !ns.plugins.is_empty());
    if !our_session_hosts_introspection_plugin(node_statuses, our_id) {
        for node_status in node_statuses {
            let pos = node_status.peers.iter().position(|id| id == our_id);
            if let Some(pos) = pos {
                node_status.peers.swap_remove(pos);
            }
            let pos = node_status.routers.iter().position(|id| id == our_id);
            if let Some(pos) = pos {
                node_status.routers.swap_remove(pos);
            }
        }
    }
}

fn our_session_hosts_introspection_plugin(node_statuses: &[NodeStatus], our_id: &ZenohId) -> bool {
    node_statuses
        .iter()
        .any(|node_status| node_status.id == *our_id)
}

#[cfg(test)]
mod tests {
    use introspection_common::v1::PluginInformation;

    use super::*;

    fn id(byte: u8) -> ZenohId {
        ZenohId::try_from(&[byte][..]).unwrap()
    }

    fn cli_info(name: &str) -> ParticipantInfo {
        ParticipantInfo {
            kind: "cli".to_owned(),
            name: name.to_owned(),
            origin: None,
        }
    }

    /// A node running the introspection plugin: it always reports at least that
    /// plugin, which is what marks the status as coming from a plugin host.
    fn plugin_status(id: ZenohId, peers: &[ZenohId]) -> NodeStatus {
        NodeStatus {
            id,
            participant: None,
            peers: peers.to_vec(),
            routers: vec![],
            plugins: vec![PluginInformation {
                name: "introspection".to_owned(),
                config: serde_json::Value::Null,
            }],
        }
    }

    /// A session with no plugin that answered for itself, as `declare_participant` does.
    fn participant_status(id: ZenohId) -> NodeStatus {
        NodeStatus {
            participant: Some(cli_info("m network status")),
            plugins: vec![],
            ..plugin_status(id, &[])
        }
    }

    #[test]
    fn drops_own_participant_row_and_stays_hidden() {
        let us = id(1);
        let node = id(2);
        let mut statuses = vec![plugin_status(node, &[us]), participant_status(us)];

        remove_connecting_node(&mut statuses, &us);

        assert_eq!(statuses.len(), 1);
        assert!(statuses[0].peers.is_empty());
    }

    #[test]
    fn keeps_participants_on_other_sessions() {
        let us = id(1);
        let other_cli = id(3);
        let mut statuses = vec![participant_status(other_cli)];

        remove_connecting_node(&mut statuses, &us);

        assert_eq!(statuses.len(), 1);
    }

    #[test]
    fn plugin_hosting_session_still_lists_itself() {
        let us = id(1);
        let mut statuses = vec![plugin_status(us, &[]), plugin_status(id(2), &[us])];

        remove_connecting_node(&mut statuses, &us);

        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[1].peers, vec![us]);
    }

    /// A node that both hosts the plugin and describes itself — a gateway —
    /// stays in its own listing, links included. Only the plugin list decides.
    #[test]
    fn self_described_plugin_host_still_lists_itself() {
        let us = id(1);
        let mut statuses = vec![
            NodeStatus {
                participant: Some(cli_info("myrmic gateway")),
                ..plugin_status(us, &[])
            },
            plugin_status(id(2), &[us]),
        ];

        remove_connecting_node(&mut statuses, &us);

        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[1].peers, vec![us]);
    }
}
