use serde::{Deserialize, Serialize};

use crate::{PortId, TaskId};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct InputRecord {
    pub(crate) id: PortId,
    pub(crate) output: OutputRecord,
}

impl InputRecord {
    #[must_use]
    pub fn new(id: impl Into<PortId>, output: OutputRecord) -> Self {
        Self {
            id: id.into(),
            output,
        }
    }

    /// Returns the record of the output from which the input is received
    #[must_use]
    pub fn from(&self) -> &OutputRecord {
        &self.output
    }

    #[must_use]
    pub fn id(&self) -> &PortId {
        &self.id
    }
}

#[derive(Debug, Serialize, Deserialize, Hash, Clone, PartialEq, Eq)]
pub struct OutputRecord {
    id: PortId,
    task_id: TaskId,
}

impl OutputRecord {
    #[must_use]
    pub fn new(id: PortId, node_id: TaskId) -> Self {
        Self {
            id,
            task_id: node_id,
        }
    }

    #[must_use]
    pub fn id(&self) -> &PortId {
        &self.id
    }

    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
}
