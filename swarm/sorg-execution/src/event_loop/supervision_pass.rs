//! The event loop's supervision arms: crash observation (spec §5) and the
//! child-side fencing verification pass (spec §3).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use cell_protocol::{PlacementKind, RuntimeId, Sri};
use sorg_common::{LostReason, node_lease, report_cell_death};
use tracing::{debug, warn};

use crate::event_loop::{CleanupAction, Runtime};
use crate::supervision::fencing::{Evidence, RowFacts, RowRead, Verdict, WatchedCell};

impl Runtime {
    /// A hosted cell's task ended. Deliberate kills remove the map entry
    /// before the task terminates, so a still-registered sri is a crash: the
    /// cell is reaped, its rows are queued for release, and its death is
    /// reported — a `cell_lost` to its parent, or (for a root) a root-death
    /// signal that drives its restart policy. Detached edges report nothing.
    pub(in crate::event_loop) fn handle_cell_exited(&mut self, sri: Sri) {
        if !self.cells.contains_key(&sri) {
            return;
        }
        let meta = self.meta.get(&sri).cloned();
        warn!(sri = %sri, instance = ?meta.as_ref().map(|m| m.gen_id), "cell crashed");
        self.kill_local(&sri);
        self.cleanup.push(CleanupAction::ReleaseCell(sri));
        self.cleanup.push(CleanupAction::EraseInstance(sri));

        let Some(meta) = meta else { return };
        let session = self.session.clone();
        tokio::spawn(async move {
            if let Err(err) = report_cell_death(
                &session,
                sri,
                meta.gen_id,
                meta.lineage.parent,
                meta.lineage.detached,
                meta.lineage.local_name,
                LostReason::Crashed,
            )
            .await
            {
                warn!("cell death report (crashed) for '{sri}' failed: {err}");
            }
        });
    }

    /// Local kill: dropping the `CellHandle` closes the poison channel and
    /// terminates the cell task. The watcher's later `CellExited` finds the
    /// map entry gone and is ignored.
    pub(in crate::event_loop) fn kill_local(&mut self, sri: &Sri) {
        self.cells.remove(sri);
        self.meta.remove(sri);
        self.fencing.forget(sri);
    }

    pub(in crate::event_loop) async fn verify_pass(&mut self) {
        if !self.sweep_done {
            self.sweep_done = self.startup_sweep().await;
        }
        self.drain_cleanup().await;

        match node_lease::list_leases(&self.session).await {
            Ok(leases) => {
                let now = Instant::now();
                for (id, lease) in leases {
                    self.lease_tracker.observe(
                        id,
                        lease.seq,
                        std::time::Duration::from_millis(lease.ttl_ms),
                        now,
                    );
                }
            }
            Err(err) => debug!("fencing: lease scan failed: {err}"),
        }

        let now = Instant::now();
        let my_exec = self.info.id();
        let watched: Vec<WatchedCell> = self.meta.values().cloned().collect();
        let mut row_cache: HashMap<Sri, RowRead<RowFacts>> = HashMap::new();

        for cell in watched {
            let self_row = self.read_row(&mut row_cache, &cell.sri, my_exec).await;
            let (parent_row, parent_lease_expired) =
                match (cell.lineage.parent, cell.lineage.detached) {
                    (Some(parent), false) => {
                        let row = self.read_row(&mut row_cache, &parent, my_exec).await;
                        let expired = match row {
                            RowRead::Ok((node, _)) => {
                                // The edge's grace overrides the parent
                                // node's declared ttl: this cell's personal
                                // tolerance for parent silence.
                                let tolerance = cell
                                    .lineage
                                    .grace_ms
                                    .map(std::time::Duration::from_millis)
                                    .or_else(|| self.lease_tracker.ttl_of(node))
                                    .unwrap_or(self.timing.ttl);
                                Some(
                                    self.lease_tracker
                                        .stale_for(node, now)
                                        .is_some_and(|stale| stale > tolerance),
                                )
                            }
                            _ => None,
                        };
                        (row, expired)
                    }
                    _ => (RowRead::Failed, None),
                };
            let evidence = Evidence {
                self_row,
                parent_row,
                parent_lease_expired,
            };
            if let Verdict::Kill(why) = self.fencing.evaluate(my_exec, &cell, &evidence) {
                warn!(
                    sri = %cell.sri,
                    instance = ?cell.gen_id,
                    why = ?why,
                    "fencing: killing cell"
                );
                self.kill_local(&cell.sri);
                self.cleanup.push(CleanupAction::ReleaseCell(cell.sri));
                self.cleanup.push(CleanupAction::EraseInstance(cell.sri));
            }
        }
    }

    /// Sweeps placement rows naming this node from a previous incarnation.
    /// Node ids are stable across restarts and this exec starts empty, so a
    /// placement naming it that it is not hosting is a remnant whose body
    /// died with the old process: the parent is notified (crashed) and the
    /// rows are released. Retried every pass until it completes cleanly;
    /// returns whether it did. Actively hosted cells are skipped, so a
    /// deploy racing the sweep is never touched.
    async fn startup_sweep(&mut self) -> bool {
        let my_exec: RuntimeId = self.info.id();
        let cells = match sorg_common::list_placements(&self.session).await {
            Ok(cells) => cells,
            Err(err) => {
                debug!("startup sweep: cell scan failed: {err}");
                return false;
            }
        };
        let hosted: HashSet<Sri> = self.cells.keys().copied().collect();
        let remnants = crate::supervision::startup::select_remnants(&cells, my_exec, &hosted);
        if remnants.is_empty() {
            return true;
        }
        warn!(
            count = remnants.len(),
            "startup sweep: releasing cells of a previous incarnation"
        );

        let mut done = true;
        for sri in remnants {
            match sorg_common::instance_registry::get_instance(&self.session, &sri).await {
                Ok(Some(instance)) => {
                    // Report precedes cleanup: an unsent report is retried next
                    // pass while the rows still exist. Roots record a root-death
                    // signal; parented cells notify the parent; detached: none.
                    if let Err(err) = report_cell_death(
                        &self.session,
                        sri,
                        instance.gen_id,
                        instance.lineage.parent,
                        instance.lineage.detached,
                        instance.lineage.local_name.clone(),
                        LostReason::Crashed,
                    )
                    .await
                    {
                        warn!("startup sweep: death report for '{sri}' failed: {err}");
                        done = false;
                        continue;
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    warn!("startup sweep: instance read for '{sri}' failed: {err}");
                    done = false;
                    continue;
                }
            }
            self.cleanup.push(CleanupAction::ReleaseCell(sri));
            self.cleanup.push(CleanupAction::EraseInstance(sri));
        }
        done
    }

    /// Attempts every owed row cleanup, keeping what still fails —
    /// a partitioned exec retries until the db is reachable again, so a
    /// stale row can never block its SRI forever.
    async fn drain_cleanup(&mut self) {
        if self.cleanup.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.cleanup);
        for action in pending {
            let result = match &action {
                CleanupAction::ReleaseCell(sri) => {
                    sorg_common::remove_placement(&self.session, sri).await
                }
                CleanupAction::EraseInstance(sri) => {
                    // A row someone else already erased counts as done.
                    sorg_common::instance_registry::erase_instance_if_present(&self.session, sri)
                        .await
                        .map(|_| ())
                }
            };
            if let Err(err) = result {
                debug!("cleanup {action:?} pending: {err}");
                self.cleanup.push(action);
            }
        }
    }

    async fn read_row(
        &self,
        cache: &mut HashMap<Sri, RowRead<RowFacts>>,
        sri: &Sri,
        my_exec: cell_protocol::RuntimeId,
    ) -> RowRead<RowFacts> {
        if let Some(hit) = cache.get(sri) {
            return *hit;
        }
        let read = match sorg_common::get_placement(&self.session, sri).await {
            Ok(Some(entry)) => {
                // Placeholder rows (mid-deploy) and bridge rows carry no
                // exec placement; the self-exec check cannot refute them, so
                // this exec's own id stands in and only the incarnation is
                // compared.
                let node = match &entry.kind {
                    PlacementKind::Wasm { runtime } => runtime.id(),
                    PlacementKind::Bridge { .. } | PlacementKind::Placeholder => my_exec,
                };
                RowRead::Ok((node, entry.gen_id))
            }
            Ok(None) => RowRead::Absent,
            Err(_) => RowRead::Failed,
        };
        cache.insert(*sri, read);
        read
    }
}
