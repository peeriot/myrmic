//! Keeps this node's tag set in step with its overlay row.
//!
//! A node has one tag set, whichever plugins it runs: the tags deciding which
//! cells it may host are the tags deciding which data it replicates. This is
//! the only writer of that set — plugins read it and react.

use std::time::Duration;

use cell_protocol::node_tags::{LiveTags, NODE_TAGS_TABLE, NodeTagOverlay, node_tags_scope};
use db_client::PolledTable;
use db_client::v1::models;
use zenoh::Session;

use crate::config::PluginConfigs;

/// Backstop cadence. An overlay arriving by replication raises no local table
/// event, so the poll — not the subscription — is what makes a change written
/// on another node take effect here.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// The tags this node's configuration asks for.
///
/// Both plugins that act on tags contribute: a node that runs an application's
/// cells is usually the one that should hold its data, and splitting the two
/// would mean a node whose data placement disagreed with its cell placement.
pub fn configured(plugins: &PluginConfigs) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();

    #[cfg(feature = "plugin-db")]
    if let Some(db) = &plugins.db {
        tags.extend(db.tags.iter().cloned());
    }

    #[cfg(feature = "plugin-execution")]
    if let Some(exec) = &plugins.execution {
        tags.extend(
            exec.capability_tags()
                .iter()
                .map(|tag| String::from(tag.as_ref())),
        );
    }

    tags
}

/// Facts about this node, which an overlay may not remove: its own runtime
/// tag, and — when it can run cells — what it can run them on.
pub fn intrinsic(session: &Session) -> Vec<String> {
    let mut tags = vec![cell_protocol::replication::runtime_tag(
        session.zid().into(),
    )];

    #[cfg(feature = "plugin-execution")]
    {
        tags.push(String::from(myrmic_tags::TAG_LINUX));

        // Advertised only when the BlueZ backend is compiled in, so a cell
        // requiring `ble` is never placed on a node that cannot serve it.
        #[cfg(feature = "ble-linux")]
        tags.push(String::from(myrmic_tags::TAG_BLE));
    }

    tags
}

/// This node's tags as a boot leaves them: what its configuration asks for.
/// [`watch`] departs from this only once a tag is written after this boot, but
/// a plugin that starts first must not see an empty set.
pub fn effective_at_boot(session: &Session, configured: &[String]) -> Vec<String> {
    cell_protocol::node_tags::effective(None, configured, &intrinsic(session))
}

/// Follows this node's overlay row, republishing the resulting tag set. Runs
/// until the session drops.
///
/// A restart hands the node's configuration back its say: the overlay found at
/// boot is dropped rather than applied, so only tags written after this boot
/// take effect. Until that row is known to be gone the node carries exactly
/// what its configuration asked for, and the drop is retried on the poll
/// cadence — an overlay left standing because the db was unreachable must not
/// come into force later.
pub async fn watch(session: Session, tags: LiveTags, configured: Vec<String>) {
    let intrinsic = intrinsic(&session);
    let db = db_client::v1::Client::new(&session);
    let node = session.zid().into();

    let polled = PolledTable::new(
        &db,
        models::Subject::Scope(node_tags_scope()),
        NODE_TAGS_TABLE,
    )
    .await;

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    let mut cleared = false;

    loop {
        match read(&db, node).await {
            Ok(overlay) => {
                let overlay = if cleared {
                    overlay
                } else {
                    cleared = match overlay {
                        Some(_) => clear(&db, node).await,
                        None => true,
                    };

                    None
                };

                let effective =
                    cell_protocol::node_tags::effective(overlay.as_ref(), &configured, &intrinsic);

                if tags.set(effective) {
                    tracing::info!("node tags: {}", tags.get().join(", "));
                }
            }
            Err(err) => tracing::warn!("unable to read this node's tag overlay: {err}"),
        }

        polled.wait(&mut interval).await;
    }
}

/// Deletes this node's overlay row, reporting whether it is gone.
async fn clear(db: &db_client::v1::Client, node: cell_protocol::RuntimeId) -> bool {
    let deleted = db
        .write_tx_in(node_tags_scope(), async move |client, tx_id| {
            client
                .send(models::tb_delete::Request {
                    id: tx_id,
                    op: models::tb_delete::Op {
                        scope: node_tags_scope(),
                        table: String::from(NODE_TAGS_TABLE),
                        eid: node.to_string().into_bytes(),
                    },
                })
                .await?
                .map_err(|err| err.message)?;

            Ok(())
        })
        .await;

    match deleted {
        Ok(()) => {
            tracing::info!("dropped this node's tag overlay: its configuration has the say");
            true
        }
        Err(err) => {
            tracing::warn!("unable to drop this node's tag overlay: {err}");
            false
        }
    }
}

/// This node's overlay, or `None` when it has never been tagged.
async fn read(
    db: &db_client::v1::Client,
    node: cell_protocol::RuntimeId,
) -> anyhow::Result<Option<NodeTagOverlay>> {
    let response = db
        .read_tx_in(node_tags_scope(), async move |client, tx_id| {
            client
                .send(models::tb_get::Request {
                    id: tx_id,
                    op: models::tb_get::Op {
                        scope: node_tags_scope(),
                        table: String::from(NODE_TAGS_TABLE),
                        eid: node.to_string().into_bytes(),
                    },
                })
                .await
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to communicate with db: {err}"))?
        .map_err(|err| anyhow::anyhow!("unable to read the node tags table: {}", err.message))?;

    response
        .value
        .map(|value| postcard::from_bytes(&value))
        .transpose()
        .map_err(|err| anyhow::anyhow!("unreadable tag overlay: {err}"))
}

#[cfg(all(test, feature = "plugin-db"))]
mod tests {
    use cell_protocol::RuntimeId;

    use super::*;
    use crate::plugins::MyrmicPlugin as _;

    /// How long a test waits on the watcher. Generous: the first routed
    /// transaction has to wait for the db plugin to declare itself the holder
    /// of the `sys` namespace.
    const PATIENCE: Duration = Duration::from_secs(15);

    /// A node with an in-memory db, the only plugin the watcher needs.
    /// Scouting is off so a stray peer on the network cannot answer for it.
    async fn start_node() -> (Session, swarm_api::DropSender) {
        let mut config = zenoh::Config::default();
        config
            .insert_json5("timestamping/enabled", "{ peer: true }")
            .unwrap();
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .unwrap();
        config
            .insert_json5("scouting/gossip/enabled", "false")
            .unwrap();

        let session = zenoh::open(config).await.expect("unable to open session");
        let (drop_tx, drop_rx) = flume::bounded(1);

        let ctx = crate::plugins::MyrmicCtx::new(
            session.clone(),
            tokio::runtime::Handle::current(),
            Default::default(),
            LiveTags::default(),
            drop_rx,
            swarm_api::Ready::default(),
        );

        crate::plugins::db::Plugin::main(ctx, Default::default())
            .await
            .expect("unable to start db plugin");

        (session, drop_tx)
    }

    /// Starts the watcher on a node configured with one tag, returning the set
    /// it publishes.
    fn start_watcher(session: &Session) -> LiveTags {
        let configured = vec![String::from("region-1")];
        let tags = LiveTags::new(effective_at_boot(session, &configured));

        tokio::spawn(watch(session.clone(), tags.clone(), configured));

        tags
    }

    /// Writes an overlay adding `tag`, retrying until the db answers.
    async fn tag(db: &db_client::v1::Client, node: RuntimeId, name: &str) {
        let mut overlay = NodeTagOverlay::new(node);
        overlay.add(name);

        let eid = overlay.key().into_bytes();
        let value = postcard::to_allocvec(&overlay).expect("an overlay should always serialise");

        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            let (eid, value) = (eid.clone(), value.clone());

            let written = db
                .write_tx_in(node_tags_scope(), async move |client, tx_id| {
                    client
                        .send(models::tb_insert::Request {
                            id: tx_id,
                            op: models::tb_insert::Op {
                                scope: node_tags_scope(),
                                table: String::from(NODE_TAGS_TABLE),
                                eid: Some(eid),
                                value,
                            },
                        })
                        .await?
                        .map_err(|err| err.message)?;

                    Ok(())
                })
                .await;

            match written {
                Ok(()) => return,
                Err(err) => assert!(
                    tokio::time::Instant::now() < deadline,
                    "unable to write an overlay: {err}"
                ),
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Waits for the overlay row to be gone, which is the watcher reporting it
    /// finished the boot drop.
    async fn await_cleared(db: &db_client::v1::Client, node: RuntimeId) {
        let deadline = tokio::time::Instant::now() + PATIENCE;

        loop {
            if let Ok(None) = read(db, node).await {
                return;
            }

            assert!(
                tokio::time::Instant::now() < deadline,
                "the overlay was still there after {PATIENCE:?}"
            );

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_boot_drops_the_overlay_it_finds() {
        let (session, _drop_tx) = start_node().await;
        let db = db_client::v1::Client::new(&session);
        let node: RuntimeId = session.zid().into();

        tag(&db, node, "gpu").await;

        let tags = start_watcher(&session);
        await_cleared(&db, node).await;

        let carried = tags.get();
        assert!(
            !carried.contains(&String::from("gpu")),
            "the dropped overlay was carried: {}",
            carried.join(", ")
        );
        assert!(carried.contains(&String::from("region-1")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_tag_written_after_the_boot_is_carried() {
        let (session, _drop_tx) = start_node().await;
        let db = db_client::v1::Client::new(&session);
        let node: RuntimeId = session.zid().into();

        // Tagged before the watcher starts, so the drop that follows is the
        // boot's — only then is a write known to land after it.
        tag(&db, node, "gpu").await;

        let tags = start_watcher(&session);
        await_cleared(&db, node).await;

        tag(&db, node, "cuda").await;

        let deadline = tokio::time::Instant::now() + PATIENCE;
        let mut retagged = tags.subscribe();

        while !tags.get().contains(&String::from("cuda")) {
            tokio::time::timeout_at(deadline, retagged.changed())
                .await
                .expect("the tag written after the boot was never carried")
                .expect("the live tag set was dropped");
        }

        assert!(
            read(&db, node)
                .await
                .expect("unable to read back")
                .is_some(),
            "the overlay written after the boot was dropped too"
        );
    }
}
