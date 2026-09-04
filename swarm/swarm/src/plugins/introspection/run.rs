use std::{collections::HashMap, sync::Arc};

use super::{
    liveliness::{CurrentNodes, monitor_nodes, set_up_liveliness_token, subscribe_to_joins},
    metrics::{NodeMetrics, publish::OtelPublisher},
};
use crate::config::PluginConfigs;
use anyhow::{Result, anyhow};
use introspection_common::v1::{
    NodeStatus, ParticipantInfo, PluginInformation, TOPIC_NODE_STATUS, topic_current_nodes,
};
use opentelemetry::InstrumentationScope;
use serde_json::Value;
use swarm_api::DropNotifier;
use tokio::sync::{Mutex, Notify};
use tracing::debug;
use zenoh::{Session, query::Query};

pub(super) async fn fallible_run(
    plugins: Arc<PluginConfigs>,
    session: Session,
    drop_rx: DropNotifier,
    config: super::Config,
    ready: Arc<Notify>,
) -> Result<()> {
    debug!("starting introspection plugin");

    let plugins_snapshot = serde_json::to_value(&plugins)
        .map_err(|err| anyhow!("Unable to represent plugins as a json value: {}", err))?;

    // 1. Subscribe to our own join topic FIRST — so we don't miss any NodeStatus
    //    messages from nodes that detect us after we declare liveliness
    let current_nodes: CurrentNodes = Arc::new(Mutex::new(HashMap::new()));
    subscribe_to_joins(&session, current_nodes.clone()).await?;

    // 2. Declare liveliness token — other nodes detect us and send their status
    //    to our join topic (which we're already listening on)
    let _live_token = set_up_liveliness_token(&session).await?;

    // 3. Start monitoring liveliness of other nodes (handles ongoing joins/leaves)
    let (_poison_snd_orch_monitor, poison_rcv_orch_monitor) = tokio::sync::oneshot::channel();
    let _monitor_handle = {
        let session = session.clone();
        let current_nodes = current_nodes.clone();
        let plugins_snapshot = plugins_snapshot.clone();
        let participant = config.participant.clone();
        tokio::task::spawn(monitor_nodes(
            poison_rcv_orch_monitor,
            session,
            current_nodes,
            plugins_snapshot,
            participant,
        ))
    };

    // 4. Declare queryables
    let status_queryable = session
        .declare_queryable(TOPIC_NODE_STATUS)
        .await
        .map_err(|err| anyhow!("failed declaring status queryable: {err:?}"))?;

    let current_nodes_topic = topic_current_nodes(session.zid())?;
    let current_nodes_queryable = session
        .declare_queryable(&current_nodes_topic)
        .await
        .map_err(|err| anyhow!("failed declaring current-nodes queryable: {err:?}"))?;

    let (_poison_snd_metrics, poison_rcv_metrics) = tokio::sync::oneshot::channel();
    let _metrics_handle = tokio::task::spawn(super::metrics::collect(
        NodeMetrics::new(Arc::new(OtelPublisher::new(
            InstrumentationScope::builder(session.zid().to_string())
                // in case relevant attributes are identified or added to the session those can be
                // added to the instrumentation scope by calling `with_attributes` here
                .build(),
        ))),
        tokio::time::interval(tokio::time::Duration::from_secs(
            config.metric_update_interval,
        )),
        poison_rcv_metrics,
    ));

    ready.notify_one();
    tracing::info!("intro plugin ready");

    loop {
        tokio::select! {
            query_result = status_queryable.recv_async() => {
                let query = query_result.map_err(|err| anyhow!("failed getting status query: {err:?}"))?;
                handle_status_request(query, &session, &plugins_snapshot, config.participant.as_ref()).await;
            }
            query_result = current_nodes_queryable.recv_async() => {
                let query = query_result.map_err(|err| anyhow!("failed getting current-nodes query: {err:?}"))?;
                handle_current_nodes_request(query, &current_nodes).await;
            }
            _ = drop_rx.recv_async() => {
                tracing::info!("Kill signal received");
                break Ok(());
            }
        }
    }
}

async fn handle_current_nodes_request(query: Query, current_nodes: &CurrentNodes) {
    let nodes: Vec<NodeStatus> = current_nodes.lock().await.values().cloned().collect();
    match serde_json::to_vec(&nodes) {
        Ok(payload) => query
            .reply(query.key_expr(), payload)
            .await
            .expect("failed to reply to current-nodes query"),
        Err(err) => query
            .reply_err(format!("failed to serialize current nodes: {err}"))
            .await
            .expect("failed to reply err to current-nodes query"),
    }
}

async fn handle_status_request(
    query: Query,
    session: &Session,
    plugins: &Value,
    participant: Option<&ParticipantInfo>,
) {
    let payload_result = get_node_status(session, plugins, participant)
        .await
        .and_then(|node_status| {
            serde_json::to_vec(&node_status)
                .map_err(|err| format!("failed to serialize node status: {err}"))
        });

    match payload_result {
        Ok(payload) => query
            .reply(query.key_expr(), payload)
            .await
            .expect("failed to reply to query"),
        Err(err) => query
            .reply_err(err)
            .await
            .expect("failed to reply err to query"),
    }
}

pub(crate) async fn get_node_status(
    session: &Session,
    plugins_value: &Value,
    participant: Option<&ParticipantInfo>,
) -> Result<NodeStatus, String> {
    let mut plugins = vec![];
    for (name, config) in plugins_value
        .as_object()
        .ok_or("plugins snapshot was not a JSON object")?
    {
        plugins.push(PluginInformation {
            name: name.to_owned(),
            config: config.clone(),
        });
    }

    Ok(NodeStatus::of_session(session, participant.cloned(), plugins).await)
}
