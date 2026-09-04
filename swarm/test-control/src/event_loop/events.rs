use zenoh::query::Query;

#[derive(Debug)]
pub enum Event {
    CreatePublisherQuery(Query),
    DeletePublisherQuery(Query),
    CreateSubscriberQuery(Query),
    DeleteSubscriberQuery(Query),
    CreateQueryableQuery(Query),
    DeleteQueryableQuery(Query),
    PutQuery(Query),
    GetQuery(Query),
    DeleteQuery(Query),
    StatsQuery(Query),
    Health(Query),
    Introspection(Query),
}
