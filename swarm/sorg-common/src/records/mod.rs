//! Records are types which are sent through zenoh between different plugins and/or runtimes

use cell_protocol::RuntimeId;
use serde::{Deserialize, Serialize};

pub(crate) mod app_deployment;
pub(crate) mod tasks;

/// Record describing an orchestration runtime
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct OrchRuntimeRecord {
    pub id: RuntimeId,
}
