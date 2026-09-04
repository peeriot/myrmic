//! Batched transactions over zenoh-nano, as the ESP runtime uses them.
//!
//! The point of an application on a device is round trips: deferring costs
//! nothing on the wire, so a cell function that only writes reaches the db
//! exactly once, when it commits. These tests count what the node actually
//! receives, which is the only measure that matters over WiFi.
#![cfg(feature = "nano")]

use embassy_time::{Duration, Timer};

use db_client::application::Application;
use db_client::v1::Client;
use db_commons::models::{self, tb_append, tb_get, tx_apply};
use zenoh_nano::ops::queryable::Queryable;

mod common;

use common::{decode, encode, with_linked_sessions};

const NODE_ID: models::NodeId = [7u8; 16];
const TX: models::TxId = (1, 2, NODE_ID);

fn scope() -> models::Scope {
    models::Scope::new("testing", "cells", "private")
}

fn append(value: &[u8]) -> tb_append::Op {
    tb_append::Op {
        scope: scope(),
        table: String::from("readings"),
        eid: None,
        value: value.to_vec(),
    }
}

/// Serves the locate for [`scope`] once, then answers every application the
/// client sends, in order — with a response, or with the refusal that rolls the
/// transaction back server-side.
async fn serve(
    session: zenoh_nano::session::Session<'static>,
    replies: &[Result<tx_apply::Response, tx_apply::Error>],
) -> Vec<tx_apply::Request> {
    let scope = scope();
    let locate_ke =
        db_commons::topics::replica_query::format(&scope.namespace, &scope.database, &scope.schema);
    let query_ke =
        db_commons::topics::format_query(uhlc::ID::try_from(&NODE_ID).expect("valid node id"));

    let mut locate_q = Queryable::declare(session, locate_ke.as_str())
        .await
        .expect("unable to declare the locate queryable");
    let mut direct_q = Queryable::declare(session, query_ke.as_str())
        .await
        .expect("unable to declare the query queryable");

    let query = locate_q.wait_for_query().await.expect("no locate query");
    let located = models::locate::Response {
        id: NODE_ID,
        head: 7,
        peers: Vec::new(),
        state: models::locate::HolderState::Replica,
    };
    locate_q
        .reply_to_query(query, Ok(encode(&located)))
        .await
        .expect("unable to reply to locate");

    let mut received = Vec::new();

    for reply in replies {
        let query = direct_q.wait_for_query().await.expect("no direct query");
        let body = query
            .body
            .clone()
            .expect("an application carries a request");

        let models::DbRequest::TxApply(application) = decode(&body) else {
            panic!("an application must travel as TxApply");
        };
        received.push(application);

        let payload = match reply {
            Ok(response) => Ok(encode(response)),
            Err(refusal) => Err(encode(refusal)),
        };

        direct_q
            .reply_to_query(query, payload)
            .await
            .expect("unable to reply to the application");
    }

    received
}

/// Deferred writes cost nothing until the commit, which carries all of them in
/// one self-committing round trip.
#[test]
fn deferred_writes_commit_in_one_round_trip() {
    with_linked_sessions(async |sess_a, sess_b| {
        let replies = [Ok(tx_apply::Response {
            tx: None,
            last: None,
        })];

        let node_side = serve(sess_a, &replies);

        let client_side = async {
            Timer::after(Duration::from_millis(300)).await;

            let mut application = Application::routed(Client::new(&sess_b), scope());

            application.defer(append(b"first")).expect("defer failed");
            application.defer(append(b"second")).expect("defer failed");
            application.defer(append(b"third")).expect("defer failed");

            application.commit().await.expect("commit failed");
        };

        let (received, ()) = embassy_futures::join::join(node_side, client_side).await;

        assert_eq!(received.len(), 1, "deferring must not reach the wire");

        let application = &received[0];
        assert_eq!(
            application.ops.len(),
            3,
            "every deferred op rides the commit"
        );
        assert_eq!(application.finish, tx_apply::Finish::Commit);
        // Nothing was flushed before, so the commit places the transaction too.
        assert!(matches!(application.target, tx_apply::Target::New { .. }));
    });
}

/// An operation whose value the caller reads back flushes what is deferred
/// before it, with itself last — the tail rule, which is also what keeps the
/// read seeing the writes that preceded it.
#[test]
fn a_read_flushes_the_writes_before_it() {
    with_linked_sessions(async |sess_a, sess_b| {
        let replies = [
            Ok(tx_apply::Response {
                tx: Some(TX),
                last: Some(tb_get::Response { value: None }.into()),
            }),
            Ok(tx_apply::Response {
                tx: None,
                last: None,
            }),
        ];

        let node_side = serve(sess_a, &replies);

        let client_side = async {
            Timer::after(Duration::from_millis(300)).await;

            let mut application = Application::routed(Client::new(&sess_b), scope());

            application
                .defer(append(b"before the read"))
                .expect("defer failed");

            let got = application
                .apply(tb_get::Op {
                    scope: scope(),
                    table: String::from("readings"),
                    eid: b"anything".to_vec(),
                })
                .await
                .expect("the read failed");
            assert!(got.value.is_none());

            application
                .defer(append(b"after the read"))
                .expect("defer failed");

            application.commit().await.expect("commit failed");
        };

        let (received, ()) = embassy_futures::join::join(node_side, client_side).await;

        assert_eq!(
            received.len(),
            2,
            "the read and the commit, and nothing else"
        );

        // The read flushed the write that preceded it, and went last itself.
        let read = &received[0];
        assert_eq!(read.ops.len(), 2);
        assert_eq!(read.ops[0].name(), "TB_APPEND");
        assert_eq!(read.ops[1].name(), "TB_GET");
        assert_eq!(read.finish, tx_apply::Finish::KeepOpen);

        // The commit continues that transaction rather than placing another.
        let commit = &received[1];
        assert_eq!(commit.ops.len(), 1);
        assert_eq!(commit.finish, tx_apply::Finish::Commit);
        assert!(matches!(commit.target, tx_apply::Target::Existing(TX)));
    });
}

/// A commit with nothing to say never reaches the wire at all: a cell function
/// that touches no db costs no round trips.
#[test]
fn an_empty_application_never_reaches_the_wire() {
    with_linked_sessions(async |_sess_a, sess_b| {
        Timer::after(Duration::from_millis(300)).await;

        let application = Application::routed(Client::new(&sess_b), scope());

        // No node is serving anything here, so a round trip could only hang or
        // fail — the commit returning at all is the assertion.
        application
            .commit()
            .await
            .expect("an empty commit should not need the network");
    });
}

/// Answers the locate, takes the one application the client sends, and never
/// replies to it — the round trip [`a_cancelled_flush_poisons_the_application`]
/// drops mid-flight. The queryable stays declared, so the client is genuinely
/// still waiting rather than seeing a query fail.
///
/// Sets `received` once the application is in hand, which is what lets the test
/// tell a cancelled *flush* from a cancelled locate that never got that far.
async fn serve_locate_then_stall(
    session: zenoh_nano::session::Session<'static>,
    received: &core::cell::Cell<bool>,
) {
    let scope = scope();
    let locate_ke =
        db_commons::topics::replica_query::format(&scope.namespace, &scope.database, &scope.schema);
    let query_ke =
        db_commons::topics::format_query(uhlc::ID::try_from(&NODE_ID).expect("valid node id"));

    let mut locate_q = Queryable::declare(session, locate_ke.as_str())
        .await
        .expect("unable to declare the locate queryable");
    let mut direct_q = Queryable::declare(session, query_ke.as_str())
        .await
        .expect("unable to declare the query queryable");

    let query = locate_q.wait_for_query().await.expect("no locate query");
    let located = models::locate::Response {
        id: NODE_ID,
        head: 7,
        peers: Vec::new(),
        state: models::locate::HolderState::Replica,
    };
    locate_q
        .reply_to_query(query, Ok(encode(&located)))
        .await
        .expect("unable to reply to locate");

    let stalled = direct_q.wait_for_query().await.expect("no direct query");
    let body = stalled
        .body
        .clone()
        .expect("an application carries a request");
    let models::DbRequest::TxApply(_) = decode(&body) else {
        panic!("an application must travel as TxApply");
    };
    received.set(true);

    core::future::pending::<()>().await;
}

/// A flush dropped mid-flight leaves an application that refuses everything
/// after it, not one that has silently lost the writes it buffered.
///
/// `flush` empties the pending buffer before the request goes out, so a
/// cancellation there — which is exactly what the ESP runtime's timeout wrapper
/// does to `apply_op` — used to drop every deferred write while leaving the
/// application healthy. The following commit then succeeded on a transaction
/// carrying none of the handler's work, with no error anywhere.
#[test]
fn a_cancelled_flush_poisons_the_application() {
    with_linked_sessions(async |sess_a, sess_b| {
        let received = core::cell::Cell::new(false);
        let node_side = serve_locate_then_stall(sess_a, &received);

        let client_side = async {
            Timer::after(Duration::from_millis(300)).await;

            let mut application = Application::routed(Client::new(&sess_b), scope());
            application
                .defer(append(b"buffered"))
                .expect("defer failed");

            {
                let read = application.apply(tb_get::Op {
                    scope: scope(),
                    table: String::from("readings"),
                    eid: b"anything".to_vec(),
                });

                // The node never answers, so the timer always wins — and
                // dropping `read` here is the cancellation under test.
                let cancelled =
                    embassy_futures::select::select(read, Timer::after(Duration::from_millis(500)))
                        .await;
                assert!(
                    matches!(cancelled, embassy_futures::select::Either::Second(())),
                    "the node is stalling; the read cannot resolve",
                );
            }

            assert!(
                received.get(),
                "the application must have reached the node, or this cancels a \
                 locate rather than a flush",
            );
            assert!(
                application.is_poisoned(),
                "a flush dropped in flight leaves the transaction's fate unknown",
            );
            application
                .defer(append(b"after the cancellation"))
                .expect_err("a poisoned application must refuse further writes");
            application
                .commit()
                .await
                .expect_err("a poisoned application must not commit");
        };

        match embassy_futures::select::select(node_side, client_side).await {
            embassy_futures::select::Either::First(()) => {
                unreachable!("the node side stalls forever")
            }
            embassy_futures::select::Either::Second(()) => (),
        }
    });
}
