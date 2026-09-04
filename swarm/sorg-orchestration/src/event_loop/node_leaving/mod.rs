use zenoh::config::ZenohId;

use crate::{Result, event_loop::Runtime, state::StateUpdate};

impl Runtime {
    pub(crate) async fn handle_leaving_node(&self, zid: ZenohId) -> Result<()> {
        let knows_orch = self.state.lock().await.knows_orch(zid);

        if knows_orch {
            self.handle_leaving_orch(zid).await;
        }

        Ok(())
    }

    async fn handle_leaving_orch(&self, node_id: ZenohId) {
        let state_update = StateUpdate::OrchLeave(node_id);
        self.state.lock().await.update_state(&state_update);
    }
}
