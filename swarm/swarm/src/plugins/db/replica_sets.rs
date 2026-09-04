//! Applies the configured replication sets to this node.
//!
//! The configuration lives in the `sys` namespace, which every node replicates
//! unconditionally, so all nodes converge on the same set of entries and each
//! decides its own participation from its own tags. Selectors are resolved here
//! rather than where they were written, so an entry naming an application picks
//! up cells deployed into it later.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use cell_protocol::node_tags::LiveTags;
use cell_protocol::replication::{
    CUSTODY_TABLE, CustodyRow, REPLICATION_TABLE, ReplicaEntry, ReplicaSelector, custody_winner,
    replication_scope,
};
use cell_protocol::{PLACEMENT_TABLE, PlacementEntry, placement_scope};
use db::store::TransactionOptions;
use db_client::PolledTable;
use db_commons::models;
use db_commons::models::locate::HolderState;

use super::{OffloadKind, StoreContext};

/// Backstop cadence. Entries arriving by replication raise no local table
/// event, so the poll — not the subscription — is what makes a remote change
/// take effect.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// How long a live full-replica peer without a custody row must be observed
/// before a provisional demotes toward it as a configured replica. A fellow
/// provisional is only recognisable once its custody row replicates here;
/// demoting before the row can arrive would let two healing provisionals
/// demote toward each other and leave the scope with no replica at all.
const CONFIGURED_GRACE: Duration = Duration::from_mins(1);

/// Namespaces this node replicates for its whole life, whatever the
/// configuration says.
///
/// `sys` carries the replication configuration itself, and `gw` the routing
/// table and assets any gateway may be asked to serve, so every node holds
/// both. `sorg` carries the cell and execution metadata, so only a node taking
/// part in orchestration or execution does.
pub fn unconditional() -> Vec<models::Subject> {
    vec![
        models::Subject::Namespace(String::from(db_commons::NAMESPACE_SYS)),
        models::Subject::Namespace(String::from(db_commons::NAMESPACE_SORG)),
        models::Subject::Namespace(String::from(cell_protocol::NAMESPACE_GATEWAY)),
    ]
}

/// Watches the replication configuration and the custody table, and keeps
/// this node's replicators in step with them. Runs until the session drops.
///
/// `tags` is this node's live set, so retagging it moves replicas without a
/// restart: a changed set wakes this loop like a changed entry does.
pub async fn run(context: StoreContext, db: db_client::v1::Client, tags: LiveTags) {
    let mut retagged = tags.subscribe();

    let polled = PolledTable::tables(
        &db,
        [
            (
                models::Subject::Scope(replication_scope()),
                REPLICATION_TABLE,
            ),
            (models::Subject::Scope(replication_scope()), CUSTODY_TABLE),
            (models::Subject::Scope(placement_scope()), PLACEMENT_TABLE),
        ],
    )
    .await;

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    let mut active: HashSet<models::Subject> = HashSet::new();
    let always = unconditional();
    // First-seen times of live full-replica peers with no custody row, for
    // the demotion grace window; see [`CONFIGURED_GRACE`].
    let mut suspects: HashMap<(models::Scope, models::NodeId), Instant> = HashMap::new();
    // When each draining scope last lost sight of every full replica, for the
    // re-arm quiet window; see [`REPLICAS_QUIET_FOR`].
    let mut quiet_since: HashMap<models::Scope, Instant> = HashMap::new();

    loop {
        match desired(&context, &db, &tags.get()).await {
            Ok(desired) => {
                reconcile(&context, &mut active, desired).await;
                custody_pass(&context, &active, &always, &mut suspects, &mut quiet_since).await;
            }
            Err(err) => tracing::warn!("unable to read replication configuration: {err}"),
        }

        tokio::select! {
            _ = polled.wait(&mut interval) => (),
            _ = retagged.changed() => (),
        }
    }
}

/// How long every full replica of a draining scope must stay out of the peer
/// view before that counts as evidence the deference target is gone. Announce
/// lag under catch-up load looks identical for a pass or two, and re-arming on
/// a blip flaps the drain into a replica and back.
const REPLICAS_QUIET_FOR: Duration = Duration::from_secs(30);

/// Applies the custody rows this node owns: re-establishes replication for
/// them (a restart forgets running replicators, never rows), converts when a
/// human has pinned this node, collapses toward a configured replica or the
/// rendezvous winner, and re-arms a drain whose deference target vanished.
async fn custody_pass(
    context: &StoreContext,
    active: &HashSet<models::Subject>,
    always: &[models::Subject],
    suspects: &mut HashMap<(models::Scope, models::NodeId), Instant>,
    quiet_since: &mut HashMap<models::Scope, Instant>,
) {
    let rows: Vec<CustodyRow> = match read_table(context, &replication_scope(), CUSTODY_TABLE) {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("unable to read the custody table: {err}");
            return;
        }
    };

    let me: models::NodeId = context.id().to_le_bytes();
    let now = Instant::now();

    let mut custodians: HashMap<&models::Scope, HashSet<models::NodeId>> = HashMap::new();
    for row in &rows {
        custodians.entry(&row.scope).or_default().insert(row.node);
    }

    // Grace entries refreshed this pass; the rest are pruned below, so a
    // flapping peer restarts its window instead of banking credit.
    let mut observed: HashSet<(models::Scope, models::NodeId)> = HashSet::new();

    for row in rows.iter().filter(|row| row.node == me) {
        let scope = &row.scope;

        let live_replicas: Vec<models::NodeId> = context
            .store
            .peer_view(scope, now)
            .into_iter()
            .filter(|peer| matches!(peer.state, HolderState::Replica))
            .map(|peer| peer.id)
            .collect();

        if context.store.is_offloading(scope) {
            // Unwinding. Every full replica for the scope staying quiet is
            // evidence the deference target is unreachable: re-arm, so the
            // drain escalates back into a replica instead of serving forever
            // (an eternal drainer would also pin the scope out of GC). The
            // quiet must persist past [`REPLICAS_QUIET_FOR`] first.
            if live_replicas.is_empty() {
                let since = *quiet_since.entry(scope.clone()).or_insert(now);
                if now.duration_since(since) >= REPLICAS_QUIET_FOR {
                    context.rearm_offload(scope);
                }
            } else {
                quiet_since.remove(scope);
            }
            continue;
        }

        let holders = custodians.get(scope);
        let (provisionals, configured_peers): (Vec<_>, Vec<_>) = live_replicas
            .into_iter()
            .filter(|id| *id != me)
            .partition(|id| holders.is_some_and(|holders| holders.contains(id)));

        let ripe_configured: Vec<models::NodeId> = configured_peers
            .into_iter()
            .filter(|id| {
                let key = (scope.clone(), *id);
                let first_seen = *suspects.entry(key.clone()).or_insert(now);
                observed.insert(key);
                now.duration_since(first_seen) >= CONFIGURED_GRACE
            })
            .collect();

        let configured_here = active
            .iter()
            .chain(always)
            .any(|subject| subject.contains(scope));

        match judge_custody(me, scope, configured_here, &ripe_configured, &provisionals) {
            CustodyAction::Hold => {
                // Idempotent; this is what re-applies custody after a restart.
                context
                    .start_replication(models::Subject::Scope(scope.clone()))
                    .await;
            }
            CustodyAction::Convert => {
                tracing::info!("configuration now carries {scope}; converting its custody");
                if let Err(err) = context.delete_own_custody_row(scope).await {
                    tracing::warn!("unable to delete the custody row of {scope}: {err}");
                }
            }
            CustodyAction::Demote { target } => {
                tracing::info!("demoting the custody of {scope}; unwinding toward a better holder");
                context
                    .store
                    .stop_replication(&models::Subject::Scope(scope.clone()));
                context.start_offload(scope.clone(), OffloadKind::Unwinding { target });
            }
        }
    }

    suspects.retain(|key, _| observed.contains(key));
}

/// What the custody watcher should do about one of this node's custody rows.
///
/// `configured` are live full-replica peers with no custody row for the scope
/// (past the grace window), `provisionals` those with one. Both already
/// exclude this node.
#[derive(Debug, PartialEq, Eq)]
enum CustodyAction {
    /// Stay (or become) the scope's provisional replica.
    Hold,
    /// Configuration now names this node; the custody row is redundant.
    Convert,
    /// A better holder exists: unwind toward it.
    Demote {
        /// The holder deferred to.
        target: models::NodeId,
    },
}

fn judge_custody(
    me: models::NodeId,
    scope: &models::Scope,
    configured_here: bool,
    configured: &[models::NodeId],
    provisionals: &[models::NodeId],
) -> CustodyAction {
    if configured_here {
        return CustodyAction::Convert;
    }

    // Intent outranks custody: a live configured replica takes the scope over
    // whatever the rendezvous draw would say.
    if let Some(target) = configured.iter().copied().max() {
        return CustodyAction::Demote { target };
    }

    // Collapse: among the live provisional lines, everyone computes the same
    // winner; the losers defer, the winner absorbs their drains.
    let lines = core::iter::once(me).chain(provisionals.iter().copied());
    match custody_winner(scope, lines) {
        Some(winner) if winner != me => CustodyAction::Demote { target: winner },
        _ => CustodyAction::Hold,
    }
}

/// Every subject this node should be replicating, per the current
/// configuration.
async fn desired(
    context: &StoreContext,
    db: &db_client::v1::Client,
    tags: &[String],
) -> anyhow::Result<HashSet<models::Subject>> {
    // The configuration lives in `sys`, which this node always replicates, so
    // it is already on local disk.
    let entries: Vec<ReplicaEntry> = read_table(context, &replication_scope(), REPLICATION_TABLE)?;

    let matched: Vec<ReplicaEntry> = entries
        .into_iter()
        .filter(|entry| entry.matches(tags))
        .collect();

    // Only the placements can say which cells an application owns; skip the
    // read when nothing selects by application.
    let placements: Vec<PlacementEntry> = if selects_an_app(&matched) {
        read_placements(db).await?
    } else {
        Vec::new()
    };

    Ok(expand(&matched, &placements, &unconditional()))
}

/// Reads the cell placements over the network rather than from the local store.
///
/// The placements live in `sorg`, which a node holds only if it takes part in
/// orchestration or execution — and a storage-only node is exactly the sort
/// that gets handed an `app:` set to replicate. Asking the network keeps those
/// nodes able to resolve one.
async fn read_placements(db: &db_client::v1::Client) -> anyhow::Result<Vec<PlacementEntry>> {
    let response = db
        .read_tx_in(placement_scope(), async move |client, tx_id| {
            client
                .send(models::tb_list::Request {
                    id: tx_id,
                    op: models::tb_list::Op {
                        scope: placement_scope(),
                        table: String::from(PLACEMENT_TABLE),
                        cursor: None,
                        limit: None,
                        order: None,
                    },
                })
                .await
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to reach a node holding the placements: {err}"))?
        .map_err(|err| anyhow::anyhow!("unable to list the placements: {}", err.message))?;

    Ok(response
        .entities
        .iter()
        .filter_map(|(id, value)| decode_row(id, value, "placement"))
        .collect())
}

fn selects_an_app(entries: &[ReplicaEntry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry.selector, ReplicaSelector::App(_)))
}

/// Resolves matched entries against the placements.
///
/// Subjects this node already replicates unconditionally are dropped: letting
/// configuration claim one would mean a later removal could stop a replicator
/// the node depends on for its whole life.
fn expand(
    entries: &[ReplicaEntry],
    placements: &[PlacementEntry],
    unconditional: &[models::Subject],
) -> HashSet<models::Subject> {
    let cells: Vec<_> = placements
        .iter()
        .map(|cell| (&cell.sri, cell.app.as_deref()))
        .collect();

    entries
        .iter()
        .flat_map(|entry| entry.selector.subjects(cells.iter().copied()))
        .filter(|subject| {
            let always = unconditional.contains(subject);

            if always {
                let (namespace, database, schema) = subject.as_keyexprs();
                tracing::debug!(
                    "ignoring replication entry for {namespace}/{database}/{schema}; it is always replicated",
                );
            }

            !always
        })
        .collect()
}

/// Reads and decodes every row of `table` from the local store.
fn read_table<T: serde::de::DeserializeOwned>(
    context: &StoreContext,
    scope: &models::Scope,
    table: &str,
) -> anyhow::Result<Vec<T>> {
    let mut tx = context.store.begin_local(&TransactionOptions::read())?;

    let key = db::domain::Key::new_scope(&scope.namespace, &scope.database, &scope.schema);
    let rows = tx.tb_list(key.table(table), None, None, None)?;

    Ok(rows
        .iter()
        .filter_map(|(id, value)| decode_row(id, value, table))
        .collect())
}

/// One malformed row shouldn't stall replication for the rest, so it's logged
/// and skipped rather than failing the whole read.
fn decode_row<T: serde::de::DeserializeOwned>(id: &[u8], value: &[u8], what: &str) -> Option<T> {
    match postcard::from_bytes(value) {
        Ok(row) => Some(row),
        Err(err) => {
            tracing::warn!(
                "skipping undecodable {what} row [{}]: {err}",
                String::from_utf8_lossy(id),
            );
            None
        }
    }
}

/// Starts what's newly wanted and stops what no longer is.
async fn reconcile(
    context: &StoreContext,
    active: &mut HashSet<models::Subject>,
    desired: HashSet<models::Subject>,
) {
    for subject in desired.difference(active) {
        let (namespace, database, schema) = subject.as_keyexprs();
        tracing::info!("replicating {namespace}/{database}/{schema}");
        context.start_replication(subject.clone()).await;
    }

    let mut stopped = false;
    for subject in active.difference(&desired) {
        let (namespace, database, schema) = subject.as_keyexprs();
        tracing::info!("no longer replicating {namespace}/{database}/{schema}");
        context.store.stop_replication(subject);
        stopped = true;
    }

    // A stopped subject's local data must not stay stranded; offer it up now
    // rather than waiting for the stray-scan backstop.
    if stopped {
        match context.store.stray_scopes() {
            Ok(scopes) => {
                for scope in scopes {
                    context.start_offload(scope, OffloadKind::Hidden);
                }
            }
            Err(err) => tracing::warn!("unable to scan for stray scopes: {err}"),
        }
    }

    *active = desired;
}

#[cfg(test)]
mod tests {
    use cell_protocol::{Gen, PlacementKind, Sri};

    use super::*;

    fn entry(identifier: &str, tags: &[&str]) -> ReplicaEntry {
        ReplicaEntry::new(
            identifier.parse().expect("identifier should parse"),
            tags.iter().map(|tag| String::from(*tag)).collect(),
            identifier,
        )
    }

    fn tags(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|tag| String::from(*tag)).collect()
    }

    fn placed(srn: &str, app: Option<&str>) -> PlacementEntry {
        PlacementEntry {
            sri: Sri::of_path(srn).expect("srn should resolve"),
            kind: PlacementKind::Placeholder,
            app: app.map(String::from),
            gen_id: Gen::from_parts(1, 1),
        }
    }

    fn cell_subject(srn: &str) -> models::Subject {
        let sri = Sri::of_path(srn).expect("srn should resolve");
        models::Subject::Database(
            String::from(cell_protocol::NAMESPACE_CELLS),
            sri.to_string(),
        )
    }

    fn namespace(name: &str) -> models::Subject {
        models::Subject::Namespace(String::from(name))
    }

    /// Expansion against the namespaces a node with orchestration or execution
    /// holds for its whole life.
    fn expanded(
        entries: &[ReplicaEntry],
        placements: &[PlacementEntry],
    ) -> HashSet<models::Subject> {
        expand(
            entries,
            placements,
            &[namespace("sys"), namespace("gw"), namespace("sorg")],
        )
    }

    #[test]
    fn retagging_a_node_moves_which_entries_it_holds() {
        // What `run` does each pass: read the live set, match against it. The
        // set changing is what lets a replica move without a restart.
        let entries = [
            entry("chatty", &["region-1"]),
            entry("other", &["region-2"]),
        ];
        let live = LiveTags::new(tags(&["region-1"]));

        let matched = |live: &LiveTags| -> Vec<String> {
            entries
                .iter()
                .filter(|entry| entry.matches(&live.get()))
                .map(ReplicaEntry::display_name)
                .collect()
        };

        assert_eq!(matched(&live), vec![String::from("chatty")]);

        assert!(live.set(tags(&["region-2"])));
        assert_eq!(matched(&live), vec![String::from("other")]);
    }

    #[test]
    fn only_entries_sharing_a_tag_are_expanded() {
        let entries = [
            entry("chatty", &["region-1"]),
            entry("other", &["region-2"]),
        ];
        let matched: Vec<_> = entries
            .iter()
            .filter(|entry| entry.matches(&tags(&["region-1"])))
            .cloned()
            .collect();

        assert_eq!(
            expanded(&matched, &[]),
            HashSet::from([cell_subject("chatty")])
        );
    }

    #[test]
    fn an_app_expands_through_the_placements() {
        let entries = [entry("app:chatty", &["region-1"])];
        let placements = [
            placed("chatty/server", Some("chatty")),
            placed("chatty/client", Some("chatty")),
            placed("elsewhere", Some("other")),
        ];

        assert_eq!(
            expanded(&entries, &placements),
            HashSet::from([cell_subject("chatty/server"), cell_subject("chatty/client")]),
        );
    }

    #[test]
    fn an_app_with_no_placed_cells_expands_to_nothing() {
        let entries = [entry("app:chatty", &["region-1"])];

        assert!(expanded(&entries, &[]).is_empty());
    }

    #[test]
    fn the_placements_are_only_read_when_an_app_is_selected() {
        assert!(selects_an_app(&[entry("app:chatty", &["region-1"])]));
        assert!(!selects_an_app(&[
            entry("chatty", &["region-1"]),
            entry("scope:sys/rep", &["region-1"]),
        ]));
    }

    #[test]
    fn overlapping_entries_collapse_to_one_subject() {
        let uuid = Sri::of_path("chatty").expect("srn").to_string();
        let entries = [entry("chatty", &["region-1"]), entry(&uuid, &["region-1"])];

        assert_eq!(
            expanded(&entries, &[]),
            HashSet::from([cell_subject("chatty")])
        );
    }

    #[test]
    fn unconditionally_held_namespaces_are_never_claimed_by_configuration() {
        let entries = [
            entry("scope:sys", &["region-1"]),
            entry("scope:gw", &["region-1"]),
            entry("scope:sorg", &["region-1"]),
        ];

        assert!(expanded(&entries, &[]).is_empty());
    }

    #[test]
    fn a_database_under_an_unconditional_namespace_is_still_allowed() {
        let entries = [entry("scope:sorg/cell-placement", &["region-1"])];

        assert_eq!(
            expanded(&entries, &[]),
            HashSet::from([models::Subject::Database(
                String::from("sorg"),
                String::from("cell-placement"),
            )]),
        );
    }

    #[test]
    fn a_storage_only_node_still_guards_the_namespaces_it_always_holds() {
        let entries = [
            entry("scope:sys", &["region-1"]),
            entry("scope:gw", &["region-1"]),
            entry("scope:sorg", &["region-1"]),
        ];

        // No orchestration or execution here, so `sorg` is not unconditional
        // and configuration may legitimately place it on this node.
        assert_eq!(
            expand(&entries, &[], &[namespace("sys"), namespace("gw")]),
            HashSet::from([namespace("sorg")]),
        );
    }

    fn node(n: u8) -> models::NodeId {
        [n; 16]
    }

    fn judged(
        me: models::NodeId,
        configured_here: bool,
        configured: &[models::NodeId],
        provisionals: &[models::NodeId],
    ) -> CustodyAction {
        judge_custody(
            me,
            &models::Scope::new("tele", "telemetry", "p"),
            configured_here,
            configured,
            provisionals,
        )
    }

    #[test]
    fn a_lone_custodian_holds() {
        assert_eq!(judged(node(1), false, &[], &[]), CustodyAction::Hold);
    }

    #[test]
    fn a_custodian_the_configuration_now_names_converts() {
        // Even with peers around: the human pinned this node, so the custody
        // row is redundant and only the configured entry remains.
        assert_eq!(
            judged(node(1), true, &[node(2)], &[node(3)]),
            CustodyAction::Convert,
        );
    }

    #[test]
    fn a_custodian_demotes_toward_a_live_configured_replica() {
        assert_eq!(
            judged(node(1), false, &[node(2)], &[]),
            CustodyAction::Demote { target: node(2) },
        );
        // Deterministic pick among several.
        assert_eq!(
            judged(node(1), false, &[node(2), node(4)], &[]),
            CustodyAction::Demote { target: node(4) },
        );
    }

    #[test]
    fn provisionals_collapse_to_the_rendezvous_winner() {
        let scope = models::Scope::new("tele", "telemetry", "p");
        let winner =
            cell_protocol::replication::custody_winner(&scope, [node(1), node(2)]).expect("winner");
        let loser = if winner == node(1) { node(2) } else { node(1) };

        // The loser defers to the winner; the winner holds and absorbs the
        // loser's drain.
        assert_eq!(
            judged(loser, false, &[], &[winner]),
            CustodyAction::Demote { target: winner },
        );
        assert_eq!(judged(winner, false, &[], &[loser]), CustodyAction::Hold);
    }
}
