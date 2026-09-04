use std::str::FromStr;

use anyhow::{Result, anyhow};
use zenoh::{
    config::ZenohId,
    key_expr::{KeyExpr, OwnedKeyExpr, format::keformat},
};

// Used to query the status of all nodes hosting an introspection plugin
pub const TOPIC_NODE_STATUS: &str = "@introspection/@v1/@node-status";

zenoh::key_expr::format::kedefine!(
    // Used to query the set of nodes currently known to a specific introspection plugin
    pub intro_current_nodes: "@introspection/@v1/@current-nodes/${zid:*}"
);

pub fn topic_current_nodes(our_id: ZenohId) -> Result<OwnedKeyExpr> {
    keformat!(intro_current_nodes::formatter(), zid = our_id)
        .map_err(|e| anyhow!("error formatting current-nodes topic: {e}"))
}

zenoh::key_expr::format::kedefine!(
    // Used to locally publish the info on a new node that joined the network
    pub intro_node_join:  "@introspection/@v1/@node-join/${zid:*}",
    // Used to locally publish the info on a node that left the network
    pub intro_node_leave:  "@introspection/@v1/@node-leave/${zid:*}",
    // Used to monitor liveliness of other introspection plugins (i.e., nodes)
    pub intro_liveliness: "@introspection/@liveliness/${zid:*}"
);

pub fn topic_node_join(our_id: ZenohId) -> Result<OwnedKeyExpr> {
    keformat!(intro_node_join::formatter(), zid = our_id)
        .map_err(|e| anyhow!("error formatting node join topic: {e}"))
}

pub fn topic_node_leave(our_id: ZenohId) -> Result<OwnedKeyExpr> {
    keformat!(intro_node_leave::formatter(), zid = our_id)
        .map_err(|e| anyhow!("error formatting node leave topic: {e}"))
}

pub fn topic_liveliness_own(zid: ZenohId) -> Result<OwnedKeyExpr> {
    let topic = keformat!(intro_liveliness::formatter(), zid)
        .map_err(|err| anyhow!("failed to get topic own liveliness: {err}"))?;
    Ok(topic)
}

pub fn topic_liveliness_all() -> Result<OwnedKeyExpr> {
    let topic = keformat!(intro_liveliness::formatter(), zid = "*")
        .map_err(|err| anyhow!("failed to get topic all liveliness: {err}"))?;
    Ok(topic)
}

pub fn node_id(liveliness_ke: &KeyExpr<'_>) -> Result<ZenohId> {
    let parsed = intro_liveliness::parse(liveliness_ke)
        .map_err(|err| anyhow!("failed to parse orch liveliness query ke: {err}"))?;
    let zid = parsed.zid();
    let zid =
        ZenohId::from_str(zid.as_str()).map_err(|err| anyhow!("failed to read into zid: {err}"))?;
    Ok(zid)
}
