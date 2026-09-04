//! Locate-driven routing over zenoh-nano, as ESP nodes use it: a routed
//! `tx_begin` must discover the holder via the locate queryable rather than
//! falling back to a `db_info` broadcast.
#![cfg(feature = "nano")]

use embassy_time::{Duration, Timer};

use db_client::v1::Client;
use db_commons::models::{self, locate, tx_apply, tx_begin};
use zenoh_nano::ops::queryable::Queryable;

mod common;

use common::{decode, encode, with_linked_sessions};

#[test]
fn routed_tx_begin_locates_the_holder() {
    // The fake node's identity, and the keys it serves.
    let node_id: models::NodeId = [7u8; 16];
    let node = uhlc::ID::try_from(&node_id).expect("valid node id");
    let scope = models::Scope::new("testing", "events", "public");

    let locate_ke =
        db_commons::topics::replica_query::format(&scope.namespace, &scope.database, &scope.schema);
    let query_ke = db_commons::topics::format_query(node);

    let expected_tx: models::TxId = (1, 2, node_id);

    with_linked_sessions(async |sess_a, sess_b| {
        let node_side = async {
            let mut locate_q = Queryable::declare(sess_a, locate_ke.as_str())
                .await
                .expect("unable to declare the locate queryable");
            let mut direct_q = Queryable::declare(sess_a, query_ke.as_str())
                .await
                .expect("unable to declare the query queryable");

            // Discovery comes first, carrying the version bound.
            let query = locate_q.wait_for_query().await.expect("no locate query");
            let body = query.body.clone().expect("locate carries a request");
            let req: locate::Request = decode(&body);
            assert_eq!(req.min_version, None);

            let response = locate::Response {
                id: node_id,
                head: 7,
                peers: Vec::new(),
                state: locate::HolderState::Replica,
            };
            locate_q
                .reply_to_query(query, Ok(encode(&response)))
                .await
                .expect("unable to reply to locate");

            // The begin must then land here directly, not as a broadcast.
            let query = direct_q.wait_for_query().await.expect("no direct query");
            let body = query.body.clone().expect("the begin carries a request");
            let req: models::DbRequest = decode(&body);
            let models::DbRequest::TxApply(application) = req else {
                panic!("a routed begin must travel as an application");
            };

            // A begin is an application that applies nothing: it places the
            // transaction against the located holder and leaves it open.
            assert!(application.ops.is_empty());
            assert_eq!(application.finish, tx_apply::Finish::KeepOpen);
            assert!(matches!(
                application.target,
                tx_apply::Target::New {
                    constraint: tx_begin::Constraint::Routed(_),
                    ..
                }
            ));

            let response = tx_apply::Response {
                tx: Some(expected_tx),
                last: None,
            };
            direct_q
                .reply_to_query(query, Ok(encode(&response)))
                .await
                .expect("unable to reply to the begin");
        };

        let client_side = async {
            // Give the node a moment to declare its queryables.
            Timer::after(Duration::from_millis(300)).await;

            let client = Client::new(&sess_b);

            let response = client
                .send(tx_begin::Request::routed(scope.clone()))
                .await
                .expect("transport failed")
                .expect("tx begin failed");

            assert_eq!(response.id, expected_tx);
        };

        embassy_futures::join::join(node_side, client_side).await;
    });
}
