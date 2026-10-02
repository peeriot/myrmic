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
    /// A hosted cell's task ended. It was a crash only if that incarnation is
    /// still the one registered in the event loop's map: deliberate undeploys
    /// remove the entry before the task terminates, and a deploy over a
    /// still-hosted sri replaces it with the successor's.
    CellExited(cell_protocol::Sri, cell_protocol::Gen),
    /// Periodic supervision tick: drain pending registry cleanup and run the
    /// fencing verification pass (spec §3).
    VerifyPass,
}
