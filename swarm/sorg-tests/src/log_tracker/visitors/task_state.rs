use std::str::FromStr;

use cell_protocol::RuntimeId;
use sorg_common::TaskId;
use tracing::field::Visit;
use zenoh::config::ZenohId;

use crate::{StateTracker, TaskStatus, log_tracker::TaskInfo};

/// Visitor that extracts task state changes from tracing events.
///
/// This visitor processes events containing task lifecycle fields and extracts
/// information about task initialization, starting, and deletion.
///
/// # Examples
///
/// ## Event: Task initialization
/// ```rust
/// trace!(deployment = "init", runtime_id = %rt_id, depl_id = %depl_id, task_id = %task_id, node_id = %node_id, "task init");
/// ```
/// **Result**: `runtime_id = Some(rt_id)`, `depl_id = Some(depl_id)`, `task_id = Some(task_id)`, `status = Some(TaskStatus::Init)`, `node_id = Some(node_id)`
///
/// ## Event: Task start
/// ```rust
/// trace!(deployment = "start", runtime_id = %rt_id, depl_id = %depl_id, task_id = %task_id, node_id = %node_id, "task start");
/// ```
/// **Result**: `runtime_id = Some(rt_id)`, `depl_id = Some(depl_id)`, `task_id = Some(task_id)`, `status = Some(TaskStatus::Running)`, `node_id = Some(node_id)`
///
/// ## Event: Task deletion
/// ```rust
/// trace!(deployment = "delete", runtime_id = %rt_id, depl_id = %depl_id, task_id = %task_id, node_id = %node_id, "task delete");
/// ```
/// **Result**: `runtime_id = Some(rt_id)`, `depl_id = Some(depl_id)`, `task_id = Some(task_id)`, `status = Some(TaskStatus::Deleted)`, `node_id = Some(node_id)`
///
/// In all other cases, the event is either completely ignored or does not contribute to changing the state
/// of the tracker. The tracker handler only processes events where ALL required fields are present.
#[derive(Debug, Default)]
pub(crate) struct VisitorTaskState {
    pub(crate) runtime_id: Option<RuntimeId>,
    pub(crate) depl_id: Option<String>,
    pub(crate) task_id: Option<TaskId>,
    pub(crate) status: Option<TaskStatus>,
    pub(crate) node_id: Option<String>,
}

impl Visit for VisitorTaskState {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "deployment" {
            match value {
                "init" => {
                    self.status = Some(TaskStatus::Init);
                }
                "start" => {
                    self.status = Some(TaskStatus::Running);
                }
                "delete" => {
                    self.status = Some(TaskStatus::Deleted);
                }
                _ => {}
            }
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let value_str = format!("{:?}", value);
        match field.name() {
            "runtime_id" => {
                let zenoh_id = ZenohId::from_str(&value_str).unwrap();
                self.runtime_id = Some(RuntimeId::from(zenoh_id));
            }
            "depl_id" => {
                self.depl_id = Some(value_str);
            }
            "task_id" => {
                self.task_id = Some(TaskId::try_from(value_str).unwrap());
            }
            "node_id" => {
                self.node_id = Some(value_str);
            }
            _ => {}
        }
    }
}

impl StateTracker {
    pub(crate) fn handle_task_state_change(&self, visitor_tasks: VisitorTaskState) {
        if let (Some(runtime_id), Some(depl_id), Some(task_id), Some(status), Some(node_id)) = (
            visitor_tasks.runtime_id,
            visitor_tasks.depl_id,
            visitor_tasks.task_id,
            visitor_tasks.status,
            visitor_tasks.node_id,
        ) {
            let mut tasks = self.tasks.lock().unwrap();
            match status {
                TaskStatus::Init => {
                    let task_info = TaskInfo {
                        runtime_id,
                        depl_id,
                        task_id,
                        status,
                        node_id: node_id.clone(),
                    };

                    tasks.push(task_info);
                }
                TaskStatus::Running => {
                    let task = tasks.iter_mut().find(|task| {
                        task.depl_id == depl_id
                            && task.task_id == task_id
                            && task.node_id == node_id
                    });
                    if let Some(task) = task {
                        task.status = TaskStatus::Running;
                    } else {
                        panic!("task not found");
                    }
                }
                TaskStatus::Deleted => {
                    // remove the task from the list
                    tasks.retain(|task| {
                        task.depl_id != depl_id
                            || task.task_id != task_id
                            || task.node_id != node_id
                    });
                }
            }
        }
    }
}
