//! Adds and removes tags on nodes without restarting them.
//!
//! Tags decide both which cells a node may run and which data it replicates,
//! so this is one set per node. Configuration still supplies a node's starting
//! tags; the entries written here say what to carry on top of them and what to
//! drop, and live in the `sys` namespace, which every db node replicates, so a
//! change reaches the node it names by itself.
//!
//! A node applies its entry when it next registers, so a tag can be listed
//! here before the node has picked it up — shown as pending until it has.
//!
//! An entry lasts as long as the run it was written for: a node drops its own
//! at boot, so a restart returns it to its configured tags exactly as
//! `--reset` does. A tag that must outlive a restart belongs in the node's
//! configuration.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::IsTerminal as _;

use anyhow::Result;
use cell_protocol::node_tags::{NODE_TAGS_TABLE, NodeTagOverlay, node_tags_scope};
use cell_protocol::replication::check_user_tag;
use cell_protocol::{ExecRuntimeInfo, RuntimeId};
use db_client::v1::models;

use crate::args::Ctx;
use crate::render::{BOLD, NONE, RESET, cell, styled_id, unique_prefix_lengths, width};

/// Longest node name shown before it is cut short.
const MAX_NAME: usize = 32;

/// Longest tag list shown before it is cut short. Generous where the name is
/// not: the tags are what this command is for, and a node carrying a dozen is
/// ordinary.
const MAX_TAGS: usize = 160;

#[derive(clap::Parser)]
pub struct Tags {
    /// Nodes to retag, by name or runtime id — `@node-1`, `node-1`, `@a3f2…`
    /// or a unique id prefix. Repeatable.
    ///
    /// Given without `-t`/`-e`, shows just those nodes.
    #[clap(value_name = "NODE")]
    nodes: Vec<String>,

    /// Tag the nodes should carry. Repeatable.
    ///
    /// Lasts as long as the node's run: it drops the tag when it restarts,
    /// which returns it to the tags its configuration gives it.
    #[clap(short = 't', long = "tag", value_name = "TAG", requires = "nodes")]
    tags: Vec<String>,

    /// Tag the nodes should not carry, whatever its origin. Repeatable.
    ///
    /// A tag naming what a node *is* — its platform, its hardware — is a fact
    /// rather than a preference, and stays: the exclusion is recorded but has
    /// no effect, and the tag keeps its `(removing)` marker until `--reset` or
    /// a restart.
    #[clap(short = 'e', long = "exclude", value_name = "TAG", requires = "nodes")]
    exclude: Vec<String>,

    /// Forget every tag change made to the nodes, leaving them with the tags
    /// their configuration gives them.
    #[clap(long, requires = "nodes", conflicts_with_all = ["tags", "exclude"])]
    reset: bool,
}

pub async fn handle(ctx: Ctx, cmd: Tags) -> Result<()> {
    let session = ctx.session().await?;
    let db = db_client::v1::Client::new(&session);

    let (nodes, mut overlays) = load(ctx, &session, &db).await?;

    let targets = resolve(&cmd.nodes, &nodes)?;

    if cmd.reset || !cmd.tags.is_empty() || !cmd.exclude.is_empty() {
        let changes = if cmd.reset {
            reset(&targets, &overlays)
        } else {
            edits(&targets, &cmd.tags, &cmd.exclude, &overlays)?
        };

        apply(&db, &changes).await?;

        for change in changes {
            match change {
                Change::Set(overlay) => {
                    overlays.insert(overlay.node, overlay);
                }
                Change::Drop(node) => {
                    overlays.remove(&node);
                }
            }
        }
    }

    let shown: Vec<&Node> = if cmd.nodes.is_empty() {
        nodes.iter().collect()
    } else {
        nodes
            .iter()
            .filter(|node| targets.contains(&node.id))
            .collect()
    };

    let styled = std::io::stdout().is_terminal();
    print!("{}", render(&shown, &overlays, styled));

    Ok(())
}

/// A node on the network, with the registry entry reporting the tags it is
/// actually carrying — a node running no exec has none.
struct Node {
    id: RuntimeId,
    exec: Option<ExecRuntimeInfo>,
}

impl Node {
    fn name(&self) -> Option<&str> {
        self.exec.as_ref().and_then(ExecRuntimeInfo::name)
    }

    /// The tags this node reports carrying, or `None` when it reports nothing.
    fn carried(&self) -> Option<Vec<String>> {
        self.exec.as_ref().map(|exec| {
            exec.capabilities()
                .tags()
                .iter()
                .map(|tag| String::from(tag.as_ref()))
                .collect()
        })
    }
}

/// A pending write to the node tags table.
#[derive(Debug, PartialEq, Eq)]
enum Change {
    Set(NodeTagOverlay),
    Drop(RuntimeId),
}

/// Every node on the network, plus the tag entries written for them.
async fn load(
    ctx: Ctx,
    session: &zenoh::Session,
    db: &db_client::v1::Client,
) -> Result<(Vec<Node>, HashMap<RuntimeId, NodeTagOverlay>)> {
    let introspection = ctx.introspection(session.clone()).await;

    let (statuses, execs, overlays) = tokio::join!(
        introspection.swarm_status(),
        sorg_common::exec_registry::list_registered_execs(session),
        read_overlays(ctx, db),
    );

    let execs =
        execs.map_err(|err| anyhow::anyhow!("unable to query registered runtimes: {err}"))?;

    // A node that reported no status still counts when the registry knows it,
    // and one with no registry entry still counts as somewhere to write tags.
    let mut nodes: Vec<Node> = Vec::new();

    if let Ok(statuses) = statuses {
        let linked = statuses.iter().flat_map(|status| {
            std::iter::once(&status.id)
                .chain(&status.peers)
                .chain(&status.routers)
        });

        for id in linked {
            let id = RuntimeId::from(*id);
            if !nodes.iter().any(|node| node.id == id) {
                nodes.push(Node { id, exec: None });
            }
        }
    } else {
        crate::warn!(
            ctx,
            "unable to query network status; listing only runtimes in the registry"
        );
    }

    for exec in execs {
        match nodes.iter_mut().find(|node| node.id == exec.id()) {
            Some(node) => node.exec = Some(exec),
            None => nodes.push(Node {
                id: exec.id(),
                exec: Some(exec),
            }),
        }
    }

    // Named nodes first, alphabetically; the rest by id, so the listing is
    // stable between runs.
    nodes.sort_by(|a, b| {
        let key = |node: &Node| {
            (
                node.name().is_none(),
                node.name().unwrap_or_default().to_owned(),
                node.id.to_string(),
            )
        };
        key(a).cmp(&key(b))
    });

    Ok((nodes, overlays?))
}

async fn read_overlays(
    ctx: Ctx,
    db: &db_client::v1::Client,
) -> Result<HashMap<RuntimeId, NodeTagOverlay>> {
    let listed = db
        .read_tx_in(node_tags_scope(), async move |client, tx_id| {
            client
                .send(models::tb_list::Request {
                    id: tx_id,
                    op: models::tb_list::Op {
                        scope: node_tags_scope(),
                        table: String::from(NODE_TAGS_TABLE),
                        cursor: None,
                        limit: None,
                        order: None,
                    },
                })
                .await
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to communicate with db: {err}"))?
        .map_err(|err| anyhow::anyhow!("unable to list node tags: {}", err.message))?;

    let mut overlays = HashMap::new();

    for (id, value) in listed.entities {
        match postcard::from_bytes::<NodeTagOverlay>(&value) {
            Ok(overlay) => {
                overlays.insert(overlay.node, overlay);
            }
            Err(err) => {
                crate::warn!(
                    ctx,
                    "skipping unreadable tag entry [{}]: {}",
                    String::from_utf8_lossy(&id),
                    err
                );
            }
        }
    }

    Ok(overlays)
}

/// The nodes the user named, in the order they named them.
///
/// A node is named by its runtime id, a unique prefix of one, or the name it
/// registered under; a leading `@` is accepted on any of them, matching how a
/// replication set pins to one node.
fn resolve(targets: &[String], nodes: &[Node]) -> Result<Vec<RuntimeId>> {
    targets
        .iter()
        .map(|target| resolve_one(target, nodes))
        .collect()
}

fn resolve_one(target: &str, nodes: &[Node]) -> Result<RuntimeId> {
    let wanted = target.strip_prefix('@').unwrap_or(target);

    let matched: Vec<&Node> = nodes
        .iter()
        .filter(|node| node.name() == Some(wanted) || node.id.to_string().starts_with(wanted))
        .collect();

    match matched.as_slice() {
        [node] => Ok(node.id),
        [] => Err(anyhow::anyhow!(
            "no node called '{target}' — `m nodes` lists them"
        )),
        several => {
            let names: Vec<String> = several
                .iter()
                .map(|node| match node.name() {
                    Some(name) => format!("{name} ({})", node.id),
                    None => node.id.to_string(),
                })
                .collect();

            Err(anyhow::anyhow!(
                "'{target}' matches several nodes: {}",
                names.join(", ")
            ))
        }
    }
}

/// Applies `-t`/`-e` to each target.
///
/// `-e` records that the node must not carry the tag rather than merely
/// undoing an earlier `-t`, because whether the tag also comes from the node's
/// configuration is unknowable from here — configuration never leaves the
/// node. So an entry only grows; `--reset` and the node restarting are how one
/// goes away.
fn edits(
    targets: &[RuntimeId],
    add: &[String],
    remove: &[String],
    overlays: &HashMap<RuntimeId, NodeTagOverlay>,
) -> Result<Vec<Change>> {
    for tag in add.iter().chain(remove) {
        check_user_tag(tag).map_err(|err| anyhow::anyhow!("invalid tag '{tag}': {err}"))?;
    }

    let mut changes = Vec::new();

    for node in targets {
        let mut overlay = overlays
            .get(node)
            .cloned()
            .unwrap_or_else(|| NodeTagOverlay::new(*node));

        for tag in add {
            overlay.add(tag);
        }
        for tag in remove {
            overlay.remove(tag);
        }

        if !overlay.is_empty() {
            changes.push(Change::Set(overlay));
        }
    }

    Ok(changes)
}

/// Drops the entry of every target that has one, returning those nodes to the
/// tags their configuration gives them. A node that was never tagged needs no
/// write — a tombstone would only replicate noise.
fn reset(targets: &[RuntimeId], overlays: &HashMap<RuntimeId, NodeTagOverlay>) -> Vec<Change> {
    targets
        .iter()
        .filter(|node| overlays.contains_key(node))
        .map(|node| Change::Drop(*node))
        .collect()
}

/// Writes every change in one transaction, so a partial apply can't leave half
/// a fleet retagged.
async fn apply(db: &db_client::v1::Client, changes: &[Change]) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }

    db.write_tx_in(node_tags_scope(), async move |client, tx_id| {
        for change in changes {
            match change {
                Change::Set(overlay) => {
                    let value = postcard::to_allocvec(overlay)
                        .expect("a tag entry should always serialise");

                    client
                        .send(models::tb_insert::Request {
                            id: tx_id,
                            op: models::tb_insert::Op {
                                scope: node_tags_scope(),
                                table: String::from(NODE_TAGS_TABLE),
                                eid: Some(overlay.key().into_bytes()),
                                value,
                            },
                        })
                        .await?
                        .map_err(|err| {
                            format!("unable to tag {}: {}", overlay.node, err.message)
                        })?;
                }
                Change::Drop(node) => {
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
                        .map_err(|err| format!("unable to untag {node}: {}", err.message))?;
                }
            }
        }

        Ok(())
    })
    .await
    .map_err(|err| anyhow::anyhow!("unable to write node tags: {err}"))
}

/// How a node's tags read to a user: what it reports carrying, with anything
/// written for it but not yet reflected marked as still on its way.
///
/// A node picks its entry up when it next registers — promptly on linux, on
/// the registration round for an embedded node — so the two disagree for a
/// while after every change, and saying so beats looking wrong.
///
/// A removal a node will always refuse, because the tag is a fact about it,
/// therefore reads as `(removing)` forever. Only the node knows which of its
/// tags are facts, so that is not distinguishable from one still in flight.
fn tag_cells(node: &Node, overlay: Option<&NodeTagOverlay>) -> Vec<String> {
    let Some(carried) = node.carried() else {
        // Nothing reports this node's tags, so the entry is all there is.
        return overlay.map_or_else(Vec::new, |overlay| {
            overlay
                .added
                .iter()
                .map(|tag| format!("{tag} (pending)"))
                .collect()
        });
    };

    let mut cells: Vec<String> = carried
        .iter()
        .map(|tag| match overlay {
            Some(overlay) if overlay.removed.contains(tag) => format!("{tag} (removing)"),
            _ => tag.clone(),
        })
        .collect();

    if let Some(overlay) = overlay {
        cells.extend(
            overlay
                .added
                .iter()
                .filter(|tag| !carried.contains(tag))
                .map(|tag| format!("{tag} (pending)")),
        );
    }

    cells
}

fn render(nodes: &[&Node], overlays: &HashMap<RuntimeId, NodeTagOverlay>, styled: bool) -> String {
    if nodes.is_empty() {
        return String::from("no nodes found\n");
    }

    let ids: Vec<String> = nodes.iter().map(|node| node.id.to_string()).collect();
    let uniq = unique_prefix_lengths(&ids);

    let rows: Vec<(String, String, usize, String)> = nodes
        .iter()
        .zip(&ids)
        .zip(&uniq)
        .map(|((node, id), &uniq_len)| {
            let (id, id_width) = styled_id(id, uniq_len, styled);
            let tags = tag_cells(node, overlays.get(&node.id));
            let tags = if tags.is_empty() {
                String::from(NONE)
            } else {
                cell(&tags.join(", "), MAX_TAGS)
            };

            (
                cell(node.name().unwrap_or(NONE), MAX_NAME),
                id,
                id_width,
                tags,
            )
        })
        .collect();

    let name_width = width(rows.iter().map(|(name, ..)| name.as_str())).max("NODE".len());
    let id_width = rows.iter().map(|(_, _, w, _)| *w).max().unwrap_or(2);

    let (bold, reset) = if styled { (BOLD, RESET) } else { ("", "") };

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{bold}{:<name_width$}  {:<id_width$}  TAGS{reset}",
        "NODE", "ID"
    );

    for (name, id, width, tags) in rows {
        let pad = " ".repeat(id_width - width);
        let _ = writeln!(out, "{name:<name_width$}  {id}{pad}  {tags}");
    }

    out
}

#[cfg(test)]
mod tests;
