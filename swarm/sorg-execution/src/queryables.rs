//! Module for the queryables that the execution plugin serves

use sorg_common::{
    QueryableTrait, TOPIC_EXEC_RUNTIMES, topic_execution_cell_deploy, topic_execution_cell_undeploy,
};
use zenoh::{Session, query::Query};

use crate::Event;

pub(super) enum Queryable {
    Capabilities,
    CellDeploy,
    CellUndeploy,
}

impl QueryableTrait for Queryable {
    type EventLoopEvent = Event;

    fn name(&self) -> &'static str {
        match self {
            Queryable::Capabilities => "capabilities exec plugin",
            Queryable::CellDeploy => "cell deploy",
            Queryable::CellUndeploy => "cell undeploy",
        }
    }

    fn topic(&self, session: &Session) -> String {
        match self {
            Queryable::Capabilities => TOPIC_EXEC_RUNTIMES.to_owned(),
            Queryable::CellDeploy => topic_execution_cell_deploy(session.zid()),
            Queryable::CellUndeploy => topic_execution_cell_undeploy(session.zid()),
        }
    }

    fn event_from_query(&self, query: Query) -> Self::EventLoopEvent {
        match self {
            Queryable::Capabilities => Event::InfoQuery(query),
            Queryable::CellDeploy => Event::CellDeployQuery(query),
            Queryable::CellUndeploy => Event::CellUndeployQuery(query),
        }
    }
}
