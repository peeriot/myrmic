//! Module for the queryables that the execution plugin serves

use sorg_common::{
    QueryableTrait, TOPIC_ORCH_APP_DELETE, TOPIC_ORCH_CELL_DEPLOY, TOPIC_ORCH_CELL_UNDEPLOY,
    TOPIC_ORCH_RUNTIMES,
};
use zenoh::{Session, query::Query};

use crate::Event;

pub(super) enum Queryable {
    Capabilities,
    CellDeploy,
    CellUndeploy,
    AppDelete,
}

impl QueryableTrait for Queryable {
    type EventLoopEvent = Event;

    fn name(&self) -> &'static str {
        match self {
            Queryable::Capabilities => "capabilities orch plugin",
            Queryable::CellDeploy => "cell deploy",
            Queryable::CellUndeploy => "cell undeploy",
            Queryable::AppDelete => "app delete",
        }
    }

    fn topic(&self, _session: &Session) -> String {
        match self {
            Queryable::Capabilities => TOPIC_ORCH_RUNTIMES.to_owned(),
            Queryable::CellDeploy => TOPIC_ORCH_CELL_DEPLOY.to_owned(),
            Queryable::CellUndeploy => TOPIC_ORCH_CELL_UNDEPLOY.to_owned(),
            Queryable::AppDelete => TOPIC_ORCH_APP_DELETE.to_owned(),
        }
    }

    fn event_from_query(&self, query: Query) -> Self::EventLoopEvent {
        match self {
            Queryable::Capabilities => Event::InfoQuery(query),
            Queryable::CellDeploy => Event::CellDeployQuery(query),
            Queryable::CellUndeploy => Event::CellUndeployQuery(query),
            Queryable::AppDelete => Event::AppDeleteQuery(query),
        }
    }
}
