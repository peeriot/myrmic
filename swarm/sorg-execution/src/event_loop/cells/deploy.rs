use sorg_common::{SorgPayload, WasmCellDeployRequest, bail, zenoh_err};
use tracing::debug;
use zenoh::query::Query;

use crate::supervision::fencing::WatchedCell;
use crate::{Result, event_loop::cells::CellHandle, wasm::cell::run_cell};

use crate::event_loop::{Event, Runtime};

impl Runtime {
    pub(in crate::event_loop) async fn deploy_cell(&mut self, query: Query) -> Result<()> {
        match self.deploy_cell_inner(&query).await {
            Ok(()) => {
                query
                    .reply(query.key_expr(), vec![])
                    .await
                    .map_err(|zen_err| {
                        zenoh_err!("exec failed to reply to cell deploy query", zen_err)
                    })?;
            }
            Err(err) => {
                let err_msg = err.to_string();
                let _ = query.reply_err(err_msg.as_bytes().to_vec()).await;
                tracing::warn!("{err_msg}");
            }
        }
        Ok(())
    }

    async fn deploy_cell_inner(&mut self, query: &Query) -> Result<()> {
        let Some(payload) = query.payload() else {
            bail!("cell deploy query without payload");
        };
        let request =
            WasmCellDeployRequest::from_payload(payload, "exec: deser cell deploy request")?;
        let sri = request.cell_sri;
        let lineage = request.lineage;
        // Optional payload delivered to the cell's `#[init]` (set by a spawner).
        let arguments = request.arguments;

        debug!(
            "deploying cell '{sri}' from class '{class}'",
            class = request.class_name
        );
        let (poison_snd, handle) = run_cell(
            &self.wasm_environment,
            &self.session,
            sri,
            &request.class_name,
            request.gen_id,
            lineage.clone(),
            arguments,
            self.mailbox_poll_interval,
            self.mailbox_batch_size,
        )
        .await?;
        // The watcher owns the cell task's handle and reports its exit; the
        // event loop classifies it (still in the map = crash).
        let events = self.events.clone();
        let watcher = tokio::spawn(async move {
            let _ = handle.await;
            let _ = events.send(Event::CellExited(sri)).await;
        });
        self.cells.insert(sri, CellHandle::new(poison_snd, watcher));
        self.meta.insert(
            sri,
            WatchedCell {
                sri,
                gen_id: request.gen_id,
                lineage,
            },
        );

        Ok(())
    }
}
