//! Module implementing the state of the orch plugin which is shared across the different tasks

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use zenoh::{Session, config::ZenohId};

use crate::{Error, Result, state::orch_cluster::OrchCluster};

mod orch_cluster;

pub type State = Arc<Mutex<StateInner>>;

#[must_use]
pub fn init_state(session: &Session) -> State {
    Arc::new(Mutex::new(StateInner::new(session)))
}

#[derive(Deserialize, Serialize, Clone)]
pub(crate) enum StateUpdate {
    OrchLeave(ZenohId),
}

pub struct StateInner {
    orch_cluster: OrchCluster,
    error: Option<Error>,
}

impl StateInner {
    fn new(session: &Session) -> Self {
        let own_id = session.zid();

        let orch_cluster = OrchCluster::new(own_id);
        Self {
            orch_cluster,
            error: None,
        }
    }

    fn check_error(&mut self) -> Result<()> {
        if let Some(err) = self.error.take() {
            Err(err)
        } else {
            Ok(())
        }
    }

    pub fn is_leader(&mut self) -> Result<bool> {
        self.check_error()?;
        Ok(self.orch_cluster.is_leader())
    }

    pub(crate) fn knows_orch(&self, zid: ZenohId) -> bool {
        self.orch_cluster.contains_member(zid)
    }

    pub(crate) fn update_state(&mut self, state_update: &StateUpdate) {
        match state_update {
            StateUpdate::OrchLeave(zid) => self.remove_orch(*zid),
        }
    }

    pub(crate) fn add_orch(&mut self, zid: ZenohId) {
        if let Err(err) = self.orch_cluster.add_member(zid) {
            self.error = Some(err);
        }
    }

    fn remove_orch(&mut self, zid: ZenohId) {
        if let Err(err) = self.orch_cluster.remove_member(zid) {
            self.error = Some(err);
        }
    }
}
