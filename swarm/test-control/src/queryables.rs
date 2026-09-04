use test_control_common::{
    QueryableTrait, TOPIC_CREATE_PUBLISHER, TOPIC_CREATE_QUERYABLE, TOPIC_CREATE_SUBSCRIBER,
    TOPIC_DELETE, TOPIC_DELETE_PUBLISHER, TOPIC_DELETE_QUERYABLE, TOPIC_DELETE_SUBSCRIBER,
    TOPIC_GET, TOPIC_HEALTH, TOPIC_INTROSPECTION, TOPIC_PUT, TOPIC_STATS,
};
use zenoh::{Session, query::Query};

use crate::Event;

#[allow(clippy::enum_variant_names)]
pub(super) enum Queryable {
    CreatePublisher,
    DeletePublisher,
    CreateSubscriber,
    DeleteSubscriber,
    CreateQueryable,
    DeleteQueryable,
    Put,
    Get,
    Delete,
    Stats,
    Health,
    Introspection,
}

impl QueryableTrait for Queryable {
    type EventLoopEvent = Event;

    fn name(&self) -> &'static str {
        match self {
            Queryable::CreatePublisher => "create publisher",
            Queryable::DeletePublisher => "delete publisher",
            Queryable::CreateSubscriber => "create subscriber",
            Queryable::DeleteSubscriber => "delete subscriber",
            Queryable::CreateQueryable => "create queryable",
            Queryable::DeleteQueryable => "delete queryable",
            Queryable::Put => "put data",
            Queryable::Get => "get data",
            Queryable::Delete => "delete data",
            Queryable::Stats => "request stats",
            Queryable::Health => "request health",
            Queryable::Introspection => "request introspection",
        }
    }

    fn topic(&self, _session: &Session) -> String {
        match self {
            Queryable::CreatePublisher => TOPIC_CREATE_PUBLISHER.to_owned(),
            Queryable::DeletePublisher => TOPIC_DELETE_PUBLISHER.to_owned(),
            Queryable::CreateSubscriber => TOPIC_CREATE_SUBSCRIBER.to_owned(),
            Queryable::DeleteSubscriber => TOPIC_DELETE_SUBSCRIBER.to_owned(),
            Queryable::CreateQueryable => TOPIC_CREATE_QUERYABLE.to_owned(),
            Queryable::DeleteQueryable => TOPIC_DELETE_QUERYABLE.to_owned(),
            Queryable::Put => TOPIC_PUT.to_owned(),
            Queryable::Get => TOPIC_GET.to_owned(),
            Queryable::Delete => TOPIC_DELETE.to_owned(),
            Queryable::Stats => TOPIC_STATS.to_owned(),
            Queryable::Health => TOPIC_HEALTH.to_owned(),
            Queryable::Introspection => TOPIC_INTROSPECTION.to_owned(),
        }
    }

    fn event_from_query(&self, query: Query) -> Self::EventLoopEvent {
        match self {
            Queryable::CreatePublisher => Event::CreatePublisherQuery(query),
            Queryable::DeletePublisher => Event::DeletePublisherQuery(query),
            Queryable::CreateSubscriber => Event::CreateSubscriberQuery(query),
            Queryable::DeleteSubscriber => Event::DeleteSubscriberQuery(query),
            Queryable::CreateQueryable => Event::CreateQueryableQuery(query),
            Queryable::DeleteQueryable => Event::DeleteQueryableQuery(query),
            Queryable::Put => Event::PutQuery(query),
            Queryable::Get => Event::GetQuery(query),
            Queryable::Delete => Event::DeleteQuery(query),
            Queryable::Stats => Event::StatsQuery(query),
            Queryable::Health => Event::Health(query),
            Queryable::Introspection => Event::Introspection(query),
        }
    }
}
