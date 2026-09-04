//! Module describing the fields of the state tracker and the api used to query the recorded state

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use cell_protocol::RuntimeId;
use sorg_common::{DeploymentId, TaskId};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const POLL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub(crate) struct TaskInfo {
    pub(crate) runtime_id: RuntimeId,
    pub(crate) depl_id: String,
    pub(crate) task_id: TaskId,
    pub(crate) status: TaskStatus,
    pub(crate) node_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Init,
    Running,
    Deleted,
}

pub struct StateTracker {
    pub(crate) leader_state: Arc<Mutex<HashMap<String, bool>>>,
    pub(crate) tasks: Arc<Mutex<Vec<TaskInfo>>>,
    pub(crate) module_output: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl StateTracker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            leader_state: Arc::new(Mutex::new(HashMap::new())),
            tasks: Arc::new(Mutex::new(vec![])),
            module_output: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Reset all internal state to initial values
    pub fn reset_state(&mut self) {
        self.leader_state.lock().unwrap().clear();
        self.tasks.lock().unwrap().clear();
        self.module_output.lock().unwrap().clear();
    }

    // --- Immediate (non-polling) queries, used by unit tests and Layer handlers ---

    #[must_use]
    pub fn get_leader_info(&self) -> HashMap<String, bool> {
        self.leader_state.lock().unwrap().clone()
    }

    #[must_use]
    pub fn get_modules_with_output(&self) -> Vec<String> {
        self.module_output.lock().unwrap().keys().cloned().collect()
    }

    #[must_use]
    pub fn get_output_of_module(&self, module_id: &str) -> Option<Vec<String>> {
        let map = self.module_output.lock().unwrap();
        map.get(module_id).cloned()
    }

    #[must_use]
    pub fn get_current_leader(&self) -> Option<String> {
        self.leader_state
            .lock()
            .unwrap()
            .iter()
            .find(|&(_, &is_leader)| is_leader)
            .map(|(node_id, _)| node_id.clone())
    }

    #[must_use]
    pub(crate) fn check_is_leader(&self, node_id: &str) -> bool {
        self.leader_state
            .lock()
            .unwrap()
            .get(node_id)
            .copied()
            .unwrap_or(false)
    }

    #[must_use]
    pub(crate) fn check_task_rt(
        &self,
        node_id: &str,
        depl_id: &DeploymentId,
        task_id: &TaskId,
    ) -> Option<RuntimeId> {
        let tasks = self.tasks.lock().unwrap();
        let depl_id_string = depl_id.to_string();
        tasks
            .iter()
            .find(|task| {
                task.node_id == node_id && task.depl_id == depl_id_string && task.task_id == task_id
            })
            .map(|task| task.runtime_id)
    }

    #[must_use]
    pub(crate) fn check_task_status(
        &self,
        node_id: &str,
        depl_id: &DeploymentId,
        task_id: &TaskId,
    ) -> Option<TaskStatus> {
        let tasks = self.tasks.lock().unwrap();
        let depl_id_string = depl_id.to_string();
        tasks
            .iter()
            .find(|task| {
                task.node_id == node_id && task.depl_id == depl_id_string && task.task_id == task_id
            })
            .map(|task| task.status)
    }

    #[must_use]
    pub(crate) fn check_task_num(&self) -> usize {
        self.tasks.lock().unwrap().iter().count()
    }

    // --- Async polling queries for integration tests ---

    async fn poll_until(&self, condition: impl Fn() -> bool) -> bool {
        let start = Instant::now();
        loop {
            if condition() {
                return true;
            }
            if start.elapsed() >= POLL_TIMEOUT {
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    pub async fn is_leader(&self, node_id: &str) -> bool {
        self.poll_until(|| self.check_is_leader(node_id)).await
    }

    pub async fn is_not_leader(&self, node_id: &str) -> bool {
        self.poll_until(|| !self.check_is_leader(node_id)).await
    }

    pub async fn task_on_rt(
        &self,
        node_id: &str,
        depl_id: &DeploymentId,
        task_id: &TaskId,
        expected_rt: RuntimeId,
    ) -> bool {
        self.poll_until(|| self.check_task_rt(node_id, depl_id, task_id) == Some(expected_rt))
            .await
    }

    pub async fn task_not_assigned(
        &self,
        node_id: &str,
        depl_id: &DeploymentId,
        task_id: &TaskId,
    ) -> bool {
        self.poll_until(|| self.check_task_rt(node_id, depl_id, task_id).is_none())
            .await
    }

    pub async fn task_has_status(
        &self,
        node_id: &str,
        depl_id: &DeploymentId,
        task_id: &TaskId,
        expected: TaskStatus,
    ) -> bool {
        self.poll_until(|| self.check_task_status(node_id, depl_id, task_id) == Some(expected))
            .await
    }

    pub async fn task_count_is(&self, expected: usize) -> bool {
        self.poll_until(|| self.check_task_num() == expected).await
    }
}

impl Clone for StateTracker {
    fn clone(&self) -> Self {
        Self {
            leader_state: Arc::clone(&self.leader_state),
            tasks: Arc::clone(&self.tasks),
            module_output: Arc::clone(&self.module_output),
        }
    }
}

impl Default for StateTracker {
    fn default() -> Self {
        Self::new()
    }
}
