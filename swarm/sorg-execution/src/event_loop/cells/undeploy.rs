use sorg_common::{CellUndeployRequest, SorgPayload, bail, zenoh_err};
use tracing::warn;
use zenoh::query::Query;

use crate::Result;
use crate::event_loop::Runtime;

impl Runtime {
    pub(in crate::event_loop) async fn undeploy_cell(&mut self, query: Query) -> Result<()> {
        match self.undeploy_cell_inner(&query) {
            Ok(()) => {
                query
                    .reply(query.key_expr(), vec![])
                    .await
                    .map_err(|zen_err| {
                        zenoh_err!("exec failed to reply to cell undeploy query", zen_err)
                    })?;
                Ok(())
            }
            Err(err) => {
                let err_msg = err.to_string();
                let _ = query.reply_err(err_msg.as_bytes().to_vec()).await;
                warn!("{err_msg}");
                Ok(())
            }
        }
    }

    fn undeploy_cell_inner(&mut self, query: &Query) -> Result<()> {
        let Some(payload) = query.payload() else {
            bail!("cell undeploy query without payload");
        };
        let request =
            CellUndeployRequest::from_payload(payload, "exec: deser cell undeploy request")?;

        let sri = request.cell_sri;

        // Dropping the entry closes the poison channel, which terminates the
        // cell task. Removing it BEFORE the task ends is what marks this
        // death deliberate for the exit watcher.
        match self.cells.remove(&sri) {
            Some(_) => {
                self.meta.remove(&sri);
                self.fencing.forget(&sri);
                Ok(())
            }
            None => bail!("cell '{sri}' not hosted on the exec"),
        }
    }
}
