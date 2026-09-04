use cell_protocol::Sri;

use crate::Result;
use crate::event_loop::Runtime;

impl Runtime {
    /// Undeploys a natively spawned bridge cell.
    ///
    /// Terminates the bridge cell registered under `sri` (see
    /// `deploy_cell::bridges::terminate_bridge_cell`). Returns a clean, defined error —
    /// not a panic — if no live bridge is tracked under `sri`: it never spawned, or has
    /// already been terminated.
    pub(super) fn undeploy_bridge_cell(&self, sri: &Sri) -> Result<()> {
        self.terminate_bridge_cell(sri)
    }
}
