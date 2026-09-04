use introspection_client::v1::Client;
use sorg_common::{Client as EventLoopClient, PLUGIN_NAME_ORCH, custom_err};
use tracing::debug;
use zenoh::Session;

use crate::{Event, Result};

/// Task to monitor the set of present orch and exec runtimes and generate the events expressing changes
/// of the known set of these nodes.
pub(crate) async fn monitor_orch_nodes(
    session: Session,
    el_client: EventLoopClient<Event>,
) -> Result<()> {
    let intro_client = Client::new(session).await;

    // Wait for the introspection plugin to be ready before subscribing
    intro_client
        .own_status()
        .await
        .map_err(|err| custom_err!("failed to query own introspection status: {err}"))?;

    // Subscribe FIRST so we don't miss any joins between the snapshot and the subscription
    intro_client
        .on_join(
            el_client.handle(),
            |node_status, el_client: EventLoopClient<Event>| async move {
                process_joining_node(&node_status, &el_client).await;
            },
        )
        .await
        .map_err(|err| custom_err!("orch failed to sub to nodes joining: {err}"))?;

    intro_client
        .on_leave(el_client.handle(), |node_id, el_client| async move {
            debug!("processing leaving node with id {node_id}");
            el_client
                .send(Event::NodeLeaving(node_id))
                .await
                .expect("failed to send info about node {node_id} leaving");
        })
        .await
        .map_err(|err| custom_err!("orch failed to sub to nodes leaving: {err}"))?;

    // Seed with already-present nodes (duplicates are handled by the event loop)
    let existing_nodes = intro_client
        .current_nodes()
        .await
        .map_err(|err| custom_err!("failed to get current nodes: {err}"))?;
    for node_status in existing_nodes {
        process_joining_node(&node_status, &el_client).await;
    }

    Ok(())
}

async fn process_joining_node(
    node_status: &introspection_client::v1::NodeStatus,
    el_client: &EventLoopClient<Event>,
) {
    debug!("processing joining node with id {id}", id = node_status.id);

    if node_status
        .plugins
        .iter()
        .any(|plugin_info| plugin_info.name == PLUGIN_NAME_ORCH)
    {
        el_client
            .send(Event::OrchJoining(node_status.id))
            .await
            .expect("failed to send info about joining orch");
    }
}
