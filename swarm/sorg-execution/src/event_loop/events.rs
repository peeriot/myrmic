//! Module defining the control events of the execution plugin's event loop

use zenoh::query::Query;

/// Control events of the event loop
#[derive(Debug)]
pub enum Event {
    /// Query for the execution capabilities of the execution plugin
    InfoQuery(Query),
    /// Query for deploying a cell onto this runtime
    CellDeployQuery(Query),
    /// Query for undeploying a cell from this runtime
    CellUndeployQuery(Query),
    /// A hosted cell's task ended. If the cell is still registered in the
    /// event loop's map this was a crash (deliberate undeploys remove the
    /// entry before the task terminates).
    CellExited(cell_protocol::Sri),
    /// Periodic supervision tick: drain pending registry cleanup and run the
    /// fencing verification pass (spec §3).
    VerifyPass,
}
