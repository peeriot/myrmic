//! Configures which nodes replicate which data.
//!
//! Entries are written into the `sys` namespace, which every db node
//! replicates, so the configuration reaches the whole network by itself. Each
//! node then decides whether it takes part by matching an entry's tags against
//! its own. An entry's tags may name system-stamped `@` tags — `@<runtime id>`
//! pins a replica to that one runtime.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use cell_protocol::replication::{
    CUSTODY_TABLE, CustodyRow, REPLICATION_TABLE, ReplicaEntry, ReplicaSelector, replication_scope,
};
use db_client::v1::models;

use crate::args::Ctx;
use crate::render::{BOLD, NONE, RESET, width};

#[derive(clap::Parser)]
pub struct Replicate {
    /// What to replicate: `app:<name>` for a whole application, a cell's SRI
    /// (a UUID) or SRN (`chatty`, `chatty/server`) for one cell.
    ///
    /// Given on its own, shows just that entry.
    #[clap(value_name = "IDENTIFIER")]
    target: Option<String>,

    /// Tag of the nodes that should hold a replica. Repeatable.
    ///
    /// System tags work too: `@<runtime id>` pins a replica to one runtime.
    #[clap(short = 't', long = "tag", value_name = "TAG", requires = "target")]
    tags: Vec<String>,

    /// Tag to stop replicating on. Repeatable. Removing an entry's last tag
    /// drops the entry.
    #[clap(short = 'e', long = "exclude", value_name = "TAG", requires = "target")]
    exclude: Vec<String>,

    /// Apply a file of `identifier: [tag, ...]` entries (YAML or JSON).
    ///
    /// Each identifier's tags replace whatever was configured for it; an empty
    /// list drops the entry. Identifiers the file doesn't mention are left
    /// alone unless `--prune` is given.
    #[clap(
        short = 'f',
        long,
        value_name = "PATH",
        conflicts_with_all = ["target", "tags", "exclude"],
    )]
    file: Option<PathBuf>,

    /// With `--file`, also drop entries the file doesn't mention.
    #[clap(long, requires = "file")]
    prune: bool,
}

pub async fn handle(ctx: Ctx, cmd: Replicate) -> Result<()> {
    let session = ctx.session().await?;
    let db = db_client::v1::Client::new(&session);

    let (mut entries, mut custody) = load(ctx, &db).await?;

    let changes = if let Some(path) = &cmd.file {
        from_file(path, &entries, cmd.prune)?
    } else if let Some(target) = &cmd.target {
        from_flags(target, &cmd.tags, &cmd.exclude, &entries)?
    } else {
        Vec::new()
    };

    if !changes.is_empty() {
        apply(&db, &changes).await?;
        for change in &changes {
            match change {
                Change::Set(entry) => {
                    entries.insert(entry.key(), entry.clone());
                }
                Change::Drop(key) => {
                    entries.remove(key);
                }
            }
        }
    }

    // A bare identifier is a query about that entry, not the whole table.
    let showing_one = cmd.file.is_none() && cmd.tags.is_empty() && cmd.exclude.is_empty();
    if let (true, Some(target)) = (showing_one, &cmd.target) {
        let key = parse(target)?.to_string();
        entries.retain(|entry_key, _| *entry_key == key);
        custody.retain(|row| custody_target(row) == key);
    }

    let styled = std::io::stdout().is_terminal();
    print!("{}", render(&entries, &custody, styled));

    Ok(())
}

/// The identifier a custody row files under: the canonical selector of its
/// scope, so it lands on the same display row as a configured entry for it.
fn custody_target(row: &CustodyRow) -> String {
    ReplicaSelector::Subject(models::Subject::Scope(row.scope.clone())).to_string()
}

/// The custodian as a runtime tag (`@<runtime id>`), matching how a pin of
/// that node would be written.
fn custodian_tag(row: &CustodyRow) -> String {
    match uhlc::ID::try_from(&row.node) {
        Ok(id) => format!("@{id}"),
        // An all-zero id can't round-trip through uhlc; show it raw.
        Err(_) => format!("@{:02x?}", row.node),
    }
}

/// A pending write to the replication table.
#[derive(Debug)]
enum Change {
    Set(ReplicaEntry),
    Drop(String),
}

/// Parses an identifier, mapping the failure onto the string the user wrote.
fn parse(identifier: &str) -> Result<ReplicaSelector> {
    identifier
        .parse()
        .map_err(|err| anyhow::anyhow!("invalid identifier '{identifier}': {err}"))
}

/// The configured entries, keyed by their canonical identifier, plus the
/// provisional custody rows the runtime has recorded.
async fn load(
    ctx: Ctx,
    db: &db_client::v1::Client,
) -> Result<(BTreeMap<String, ReplicaEntry>, Vec<CustodyRow>)> {
    let (configured, custody) = db
        .read_tx_in(replication_scope(), async move |client, tx_id| {
            let list = |table: &str| models::tb_list::Request {
                id: tx_id,
                op: models::tb_list::Op {
                    scope: replication_scope(),
                    table: String::from(table),
                    cursor: None,
                    limit: None,
                    order: None,
                },
            };

            let configured = client.send(list(REPLICATION_TABLE)).await?;
            let custody = client.send(list(CUSTODY_TABLE)).await?;

            Ok((configured, custody))
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to communicate with db: {err}"))?;

    let configured = configured
        .map_err(|err| anyhow::anyhow!("unable to list replication sets: {}", err.message))?;
    let custody =
        custody.map_err(|err| anyhow::anyhow!("unable to list custody rows: {}", err.message))?;

    let mut entries = BTreeMap::new();

    for (id, value) in configured.entities {
        match postcard::from_bytes::<ReplicaEntry>(&value) {
            Ok(entry) => {
                entries.insert(entry.key(), entry);
            }
            Err(err) => {
                crate::warn!(
                    ctx,
                    "skipping unreadable replication entry [{}]: {}",
                    String::from_utf8_lossy(&id),
                    err
                );
            }
        }
    }

    let mut rows = Vec::new();

    for (id, value) in custody.entities {
        match postcard::from_bytes::<CustodyRow>(&value) {
            Ok(row) => rows.push(row),
            Err(err) => {
                crate::warn!(
                    ctx,
                    "skipping unreadable custody row [{}]: {}",
                    String::from_utf8_lossy(&id),
                    err
                );
            }
        }
    }

    Ok((entries, rows))
}

/// Applies `-t`/`-e` to one identifier: added tags union in, excluded tags drop
/// out, and an entry left with no tags is removed altogether.
fn from_flags(
    target: &str,
    add: &[String],
    remove: &[String],
    entries: &BTreeMap<String, ReplicaEntry>,
) -> Result<Vec<Change>> {
    if add.is_empty() && remove.is_empty() {
        return Ok(Vec::new());
    }

    let selector = parse(target)?;
    let key = selector.to_string();

    let mut tags = entries
        .get(&key)
        .map(|entry| entry.tags.clone())
        .unwrap_or_default();

    for tag in add {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }
    tags.retain(|tag| !remove.contains(tag));

    Ok(change_for(selector, tags, target, entries)
        .into_iter()
        .collect())
}

/// Applies a file: each identifier's listed tags replace what was configured.
fn from_file(
    path: &Path,
    entries: &BTreeMap<String, ReplicaEntry>,
    prune: bool,
) -> Result<Vec<Change>> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("unable to read {}", path.display()))?;

    let wanted: BTreeMap<String, Vec<String>> = serde_yaml::from_str(&contents)
        .with_context(|| format!("unable to parse {}", path.display()))?;

    let mut changes = Vec::new();
    let mut mentioned = Vec::new();

    for (identifier, tags) in wanted {
        let selector = parse(&identifier)?;
        let key = selector.to_string();
        mentioned.push(key.clone());

        let mut deduped: Vec<String> = Vec::new();
        for tag in tags {
            if !deduped.contains(&tag) {
                deduped.push(tag);
            }
        }

        changes.extend(change_for(selector, deduped, &identifier, entries));
    }

    if prune {
        changes.extend(
            entries
                .keys()
                .filter(|key| !mentioned.contains(key))
                .map(|key| Change::Drop(key.clone())),
        );
    }

    Ok(changes)
}

/// An empty tag set means nothing would replicate the entry, so it's dropped
/// rather than stored as a set no node can match. Dropping something that was
/// never configured writes nothing — a tombstone would only replicate noise.
fn change_for(
    selector: ReplicaSelector,
    tags: Vec<String>,
    label: &str,
    entries: &BTreeMap<String, ReplicaEntry>,
) -> Option<Change> {
    let key = selector.to_string();

    if tags.is_empty() {
        entries.contains_key(&key).then_some(Change::Drop(key))
    } else {
        Some(Change::Set(ReplicaEntry::new(selector, tags, label)))
    }
}

/// Writes every change in one transaction, so a partial apply can't leave the
/// network disagreeing about what it should be replicating.
async fn apply(db: &db_client::v1::Client, changes: &[Change]) -> Result<()> {
    db.write_tx_in(replication_scope(), async move |client, tx_id| {
        for change in changes {
            match change {
                Change::Set(entry) => {
                    let value = postcard::to_allocvec(entry)
                        .expect("a replication entry should always serialise");

                    client
                        .send(models::tb_insert::Request {
                            id: tx_id,
                            op: models::tb_insert::Op {
                                scope: replication_scope(),
                                table: String::from(REPLICATION_TABLE),
                                eid: Some(entry.key().into_bytes()),
                                value,
                            },
                        })
                        .await?
                        .map_err(|err| {
                            format!("unable to store {}: {}", entry.key(), err.message)
                        })?;
                }
                Change::Drop(key) => {
                    client
                        .send(models::tb_delete::Request {
                            id: tx_id,
                            op: models::tb_delete::Op {
                                scope: replication_scope(),
                                table: String::from(REPLICATION_TABLE),
                                eid: key.clone().into_bytes(),
                            },
                        })
                        .await?
                        .map_err(|err| format!("unable to remove {key}: {}", err.message))?;
                }
            }
        }

        Ok(())
    })
    .await
    .map_err(|err| anyhow::anyhow!("unable to write replication sets: {err}"))
}

fn render(
    entries: &BTreeMap<String, ReplicaEntry>,
    custody: &[CustodyRow],
    styled: bool,
) -> String {
    if entries.is_empty() && custody.is_empty() {
        return String::from("no replication sets configured\n");
    }

    // Configured tags and provisional custodians share the target's row, so
    // divergence from intent is visible at a glance.
    let mut cells: BTreeMap<String, (String, Vec<String>)> = entries
        .values()
        .map(|entry| (entry.key(), (entry.display_name(), entry.tags.clone())))
        .collect();

    for row in custody {
        let key = custody_target(row);
        let (_, tags) = cells
            .entry(key.clone())
            .or_insert_with(|| (key.clone(), Vec::new()));
        tags.push(format!("{} (provisional)", custodian_tag(row)));
    }

    let rows: Vec<(String, String)> = cells
        .into_values()
        .map(|(target, tags)| {
            let tags = if tags.is_empty() {
                String::from(NONE)
            } else {
                tags.join(", ")
            };

            (target, tags)
        })
        .collect();

    let target_width = width(rows.iter().map(|(target, _)| target.as_str())).max("TARGET".len());

    let (bold, reset) = if styled { (BOLD, RESET) } else { ("", "") };

    let mut out = String::new();
    let _ = writeln!(out, "{bold}{:<target_width$}  TAGS{reset}", "TARGET");

    for (target, tags) in rows {
        let _ = writeln!(out, "{target:<target_width$}  {tags}");
    }

    out
}

#[cfg(test)]
mod tests;
