use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::NodeStatus;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Request {
    CreatePublisher {
        zid: String,
        key_expr: String,
        payload: String,
        count: Option<u32>,
        delay: Option<Duration>,
    },
    DeletePublisher {
        zid: String,
        pub_id: String,
    },
    CreateSubscriber {
        zid: String,
        key_expr: String,
        max_samples: Option<u32>,
        stream_key: Option<String>,
    },
    DeleteSubscriber {
        zid: String,
        sub_id: String,
    },
    CreateQueryable {
        zid: String,
        key_expr: String,
        static_payload: String,
    },
    DeleteQueryable {
        zid: String,
        qbl_id: String,
    },
    Put {
        zid: String,
        key_expr: String,
        payload: String,
    },
    Get {
        zid: String,
        key_expr: String,
        timeout_ms: Option<u64>,
    },
    Delete {
        zid: String,
        key_expr: String,
    },
    Stats {
        zid: String,
        key_expr: String,
    },
    Health {
        zid: String,
    },
    Introspection {
        zid: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Reply {
    PublisherCreated {
        ok: bool,
        pub_id: String,
        key_expr: String,
    },
    PublisherDeleted {
        ok: bool,
        pub_id: String,
    },
    SubscriberCreated {
        ok: bool,
        sub_id: String,
        key_expr: String,
    },
    SubscriberDeleted {
        ok: bool,
        sub_id: String,
    },
    QueryableCreated {
        ok: bool,
        qbl_id: String,
        key_expr: String,
    },
    QueryableDeleted {
        ok: bool,
        qbl_id: String,
    },
    Put {
        ok: bool,
        key_expr: String,
    },
    Get {
        ok: bool,
        key_expr: String,
        get_id: String,
    },
    Delete {
        ok: bool,
        key_expr: String,
    },
    Stats {
        ok: bool,
        key_expr: String,
        sent: u32,
        received: u32,
        gets: u32,
        queries: u32,
    },
    Health {
        ok: bool,
    },
    Introspection {
        nodes_status: Vec<NodeStatus>,
    },
}
