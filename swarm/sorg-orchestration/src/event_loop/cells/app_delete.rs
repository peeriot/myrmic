use cell_protocol::Sri;
use sorg_common::{SorgPayload, bail, list_placements, zenoh_err};
use tracing::warn;
use zenoh::query::Query;

use crate::{Result, event_loop::Runtime};

impl Runtime {
    /// Tears down every cell that shares the given app name (the `--app` /
    /// tree-delete path). Membership is derived from the placements — no
    /// separate app record exists — so this also catches cells spawned at
    /// runtime under the same app.
    pub(in crate::event_loop) async fn delete_app(&self, query: Query) -> Result<()> {
        let Some(payload) = query.payload() else {
            bail!("app delete query without payload");
        };
        let app_name = String::from_payload(payload, "orch: deser app delete request")?;

        let members: Vec<Sri> = list_placements(&self.session)
            .await?
            .into_iter()
            .filter(|cell| cell.app.as_deref() == Some(app_name.as_str()))
            .map(|cell| cell.sri)
            .collect();

        if members.is_empty() {
            let err_msg = format!("app '{app_name}' is not deployed");
            warn!("{err_msg}");
            let _ = query.reply_err(err_msg.as_bytes().to_vec()).await;
            return Ok(());
        }

        let mut errors = Vec::new();
        for sri in &members {
            if let Err(err) = self.undeploy_cell(sri).await {
                errors.push(format!("cell '{sri}': {err}"));
            }
        }

        if errors.is_empty() {
            query
                .reply(query.key_expr(), vec![])
                .await
                .map_err(|zen_err| {
                    zenoh_err!("orch failed to reply to app delete query", zen_err)
                })?;
        } else {
            let err_msg = format!(
                "app delete partially failed (retry to clean up): {}",
                errors.join("; ")
            );
            warn!("{err_msg}");
            let _ = query.reply_err(err_msg.as_bytes().to_vec()).await;
        }
        Ok(())
    }
}
