mod bridge;
mod embedded;
mod linux;

use cell_protocol::{ExecRuntimeInfo, PlacementKind, RuntimeKind, Sri};
use sorg_common::{
    CellUndeployRequest, SorgPayload, bail, gateway_config, get_placement, remove_placement,
    zenoh_err,
};
use tracing::warn;
use zenoh::query::Query;

use crate::Result;
use crate::event_loop::Runtime;

impl Runtime {
    pub(in crate::event_loop) async fn handle_undeploy_cell_query(
        &self,
        query: Query,
    ) -> Result<()> {
        let Some(payload) = query.payload() else {
            bail!("cell undeploy query without payload");
        };
        let request =
            CellUndeployRequest::from_payload(payload, "orch: deser cell undeploy request")?;
        let cell_sri = &request.cell_sri;

        // A single-cell undeploy always tears down just that cell — a cell can
        // always delete itself (the path `terminate_cell` uses). The "don't
        // dismantle an app one cell at a time" guard lives CLI-side, where the
        // user overrides it with an explicit flag.
        match self.undeploy_cell(cell_sri).await {
            Ok(()) => {
                query
                    .reply(query.key_expr(), vec![])
                    .await
                    .map_err(|zen_err| {
                        zenoh_err!("orch failed to reply to cell undeploy query", zen_err)
                    })?;
                Ok(())
            }
            Err(err) => {
                let err_msg = err.to_string();
                warn!("{err_msg}");
                let _ = query.reply_err(err_msg.as_bytes().to_vec()).await;
                Ok(())
            }
        }
    }

    pub(in crate::event_loop) async fn undeploy_cell(&self, cell_sri: &Sri) -> Result<()> {
        let Some(entry) = get_placement(&self.session, cell_sri).await? else {
            bail!("cell '{cell_sri}' is not deployed");
        };

        match entry.kind {
            PlacementKind::Wasm { ref runtime } => {
                // Releasing the rows below is the authoritative delete;
                // the exec teardown is only the fast-path kill. If the exec no
                // longer hosts the cell (its map was cleared by a restart) or is
                // unreachable, release the rows anyway — otherwise the corpse
                // blocks its SRI from ever being redeployed. Fencing reaps any
                // still-live remnant once its placement row is gone.
                if let Err(err) = self.undeploy_wasm_cell(cell_sri, runtime).await {
                    warn!(
                        "undeploy '{cell_sri}': exec teardown failed ({err}); releasing rows anyway"
                    );
                }
            }
            PlacementKind::Bridge { ref sri } => {
                // Bridges run on the orchestrator and are not fenced, so their
                // teardown stays strict — dropping the row on a failed teardown
                // would leak a live bridge with nothing to reap it.
                self.undeploy_bridge_cell(sri)?;
            }
            PlacementKind::Placeholder => {}
        }

        self.release_cell_resources(cell_sri).await;

        remove_placement(&self.session, &entry.sri).await?;
        // Embedded cells have no exec-side cleanup to erase their instance
        // row; for Linux cells this is a no-op race with the exec's own
        // erase. A miss leaves a corpse row the spawn gate supersedes.
        if let Err(err) =
            sorg_common::instance_registry::erase_instance(&self.session, &entry.sri).await
        {
            warn!("undeploy: erasing instance row '{cell_sri}': {err}");
        }
        Ok(())
    }

    /// Drops the resources a cell declared for itself: its gateway routes and
    /// the assets it uploaded to serve on them.
    ///
    /// Best-effort — a cell that is going away must not be kept alive by a
    /// failing cleanup. Gateways also drop routes whose owner has lost its
    /// placement, so a missed route here is corrected within a reconcile.
    async fn release_cell_resources(&self, cell_sri: &Sri) {
        match gateway_config::deregister_cell_routes(&self.session, cell_sri).await {
            Ok(mounts) if !mounts.is_empty() => {
                tracing::debug!(
                    "released gateway route(s) {} owned by '{cell_sri}'",
                    mounts.join(", ")
                );
            }
            Ok(_) => {}
            Err(err) => warn!("failed to release gateway routes for '{cell_sri}': {err}"),
        }

        match gateway_config::purge_cell_assets(&self.session, cell_sri).await {
            Ok(count) if count > 0 => {
                tracing::debug!("purged {count} gateway asset(s) owned by '{cell_sri}'");
            }
            Ok(_) => {}
            Err(err) => warn!("failed to purge gateway assets for '{cell_sri}': {err}"),
        }
    }

    pub(super) async fn teardown_cell_on_exec(
        &self,
        sri: &cell_protocol::Sri,
        kind: &PlacementKind,
    ) {
        let result = match kind {
            PlacementKind::Wasm { runtime } => self.undeploy_wasm_cell(sri, runtime).await,
            PlacementKind::Bridge { sri: bridge_sri } => self.undeploy_bridge_cell(bridge_sri),
            PlacementKind::Placeholder => return,
        };
        if let Err(err) = result {
            warn!("deploy rollback: failed to tear down cell '{sri}' on its exec: {err}");
        }
    }

    async fn undeploy_wasm_cell(&self, cell_sri: &Sri, runtime: &ExecRuntimeInfo) -> Result<()> {
        match runtime.runtime_kind() {
            RuntimeKind::Linux | RuntimeKind::Unknown => {
                self.undeploy_wasm_cell_linux(cell_sri, runtime.id()).await
            }
            RuntimeKind::Esp32c5 | RuntimeKind::Esp32c6 | RuntimeKind::Esp32c61 => {
                self.undeploy_wasm_cell_embedded(cell_sri, runtime).await
            }
        }
    }
}
