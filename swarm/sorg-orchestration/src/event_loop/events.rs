//! Module defining the control events of the orchestration plugin's event loop

use zenoh::{config::ZenohId, query::Query};

use crate::Error;

/// Control events of the event loop
#[derive(Debug)]
pub enum Event {
    /// Query for the execution capabilities of the execution plugin
    InfoQuery(Query),
    /// Sent from a a task processing event when it errors out
    ProcessError(Error),
    /// Sent from the membership monitoring task; Represents a node leaving the swarm
    NodeLeaving(ZenohId),
    /// Sent from the membership monitoring task; Represents a node hosting an orchestration plugin joining the swarm
    OrchJoining(ZenohId),
    /// Query for deploying a cell onto an execution runtime
    CellDeployQuery(Query),
    /// Query for undeploying a cell from an execution runtime
    CellUndeployQuery(Query),
    /// Query for deleting every cell that shares an app name
    AppDeleteQuery(Query),
}
