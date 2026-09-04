//! Pre-allocating replicas for the scopes a pass is about to write.
//!
//! Nothing replicates a cell's own scope or the event bus by default, so the
//! first write to each finds no holder and falls back to `any_node` — which
//! sorts node ids and pops the max, i.e. picks the *same* node for every scope
//! in the mesh. That node then answers locate as a draining sink, and a write
//! locate filters drainers out, so every subsequent write falls back to it
//! again; the sink only promotes itself after `offload_escalation_timeout`
//! (30s by default), which outlives a load pass. Measured at load 500: 69% of
//! locates failed and 99.98% of them landed on one host.
//!
//! A benchmark knows its own topology, so it can simply say up front where each
//! scope lives. That is a statement about this benchmark, not a fix for the
//! general case — production does not know which events a cell will publish.

use std::time::{Duration, Instant};

use cell_protocol::replication::{
    REPLICATION_TABLE, ReplicaEntry, ReplicaSelector, replication_scope,
};
use db_client::v1::models::{self, Scope, Subject, locate::HolderState};

/// A node learns of a new replication entry from its own 30s poll of the
/// configuration table — a replicated apply raises no local table event — so
/// the barrier has to clear that comfortably.
const CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(150);

/// How often to re-check which scopes have a replica yet.
const CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// One scope, and the host tag that should hold its replica.
pub struct Assignment {
    /// What to replicate. A cell selector covers every schema the cell owns.
    pub selector: ReplicaSelector,
    /// The concrete scope to verify convergence against — the one the pass
    /// actually writes.
    pub scope: Scope,
    /// Tag of the host that should hold the replica.
    pub tag: String,
    /// Display name for the entry, as `myrmic replicate` would show it.
    pub label: String,
}

/// Writes a replica entry per assignment, then waits until every one of those
/// scopes has a full replica answering locate.
///
/// Panics on failure: a pass that starts without its replicas measures the
/// fallback funnel instead of what it was asked to measure, and silently
/// reporting those numbers is worse than not running.
pub async fn preallocate(session: &zenoh::Session, assignments: &[Assignment]) {
    if assignments.is_empty() {
        return;
    }

    let db = db_client::v1::Client::new(session);

    println!(
        "pre-allocating replicas for {} scopes...",
        assignments.len()
    );

    db.write_tx_in(replication_scope(), async move |client, tx_id| {
        for assignment in assignments {
            let entry = ReplicaEntry::new(
                assignment.selector.clone(),
                vec![assignment.tag.clone()],
                &assignment.label,
            );
            let value =
                postcard::to_allocvec(&entry).expect("a replication entry should always serialise");

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
                    format!(
                        "unable to store replication entry {}: {}",
                        entry.key(),
                        err.message
                    )
                })?;
        }

        Ok(())
    })
    .await
    .expect("unable to write the replication configuration");

    await_replicas(session, assignments).await;
}

/// Blocks until every assigned scope has a `Replica`-state holder — the state a
/// *write* locate will accept. A sink answering as `Draining` is exactly what
/// this step exists to avoid, so it does not count.
async fn await_replicas(session: &zenoh::Session, assignments: &[Assignment]) {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    let mut pending: Vec<&Assignment> = assignments.iter().collect();

    while !pending.is_empty() {
        let mut still_pending = Vec::with_capacity(pending.len());
        for assignment in pending {
            if !has_replica(session, &assignment.scope).await {
                still_pending.push(assignment);
            }
        }
        pending = still_pending;

        if pending.is_empty() {
            break;
        }

        assert!(
            Instant::now() < deadline,
            "{} of {} scopes still have no replica after {:?}; first missing: {}",
            pending.len(),
            assignments.len(),
            CONVERGENCE_TIMEOUT,
            pending[0].label,
        );

        tokio::time::sleep(CHECK_INTERVAL).await;
    }

    println!("every pre-allocated scope has a replica");
}

async fn has_replica(session: &zenoh::Session, scope: &Scope) -> bool {
    let Ok(replica) = db_client::replica_v1::Client::new(session, Subject::Scope(scope.clone()))
    else {
        return false;
    };

    let Ok(holders) = replica.locate(scope, None).await else {
        return false;
    };

    holders
        .iter()
        .any(|holder| matches!(holder.state, HolderState::Replica))
}
