//! End-to-end tests for the central message dispatcher.
//!
//! Two in-process sessions are wired together over in-memory [`PipeLink`]s (no router, no
//! hardware), mirroring the `ping_pong` example. The tests exercise the dispatcher through the real
//! publish/subscribe/query paths: per-subscriber isolation, wildcard matching with `receive_keyed`,
//! overlapping subscriptions, queryable/get routing, and pool exhaustion.
//!
//! Run with: `cargo nextest run -p zenoh-nano` or
//! `cargo test -p zenoh-nano --test dispatch_test`.

use core::time::Duration as StdDuration;

use embassy_futures::block_on;
use embassy_futures::join::join;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer, with_timeout};

use zenoh_nano::dispatch::{Dispatch, SubscriberPool};
use zenoh_nano::link::{PipeLink, PipeLinkReceive, PipeLinkSend, PipeRead, PipeWrite};
use zenoh_nano::network::Network;
use zenoh_nano::ops::get::{Get, GetResult};
use zenoh_nano::ops::publish::Publisher;
use zenoh_nano::ops::queryable::Queryable;
use zenoh_nano::ops::subscribe::Subscriber;
use zenoh_nano::rng::{RandomSource, RngCore};
use zenoh_nano::session::{Session, SessionError, SessionResources};
use zenoh_protocol::core::ZenohIdProto;

struct TestRng;

impl RngCore for TestRng {
    fn next_u32(&mut self) -> u32 {
        rand::rng().next_u32()
    }
    fn next_u64(&mut self) -> u64 {
        rand::rng().next_u64()
    }
    fn fill_bytes(&mut self, dst: &mut [u8]) {
        rand::rng().fill_bytes(dst);
    }
}

fn bytes(buf: &zenoh_nano::buffers::ZBuf) -> Vec<u8> {
    buf.to_zslice().as_slice().to_vec()
}

/// `RandomSource` is a process-wide singleton that panics on double-init, so tests that build a
/// transport must not create one concurrently. Serialize them (nextest already isolates by process;
/// this also makes `cargo test`'s threaded runner safe). Held until the caller's `RandomSource` is
/// dropped.
fn rng_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Exhaustively exercise the dispatcher end-to-end over a pipe-connected session pair.
///
/// Everything runs inside a single `block_on` so the global `RandomSource` is created and dropped
/// exactly once (it is a process-wide singleton).
#[test]
fn dispatcher_end_to_end() {
    let _ = env_logger::builder().is_test(true).try_init();
    let _rng_guard = rng_guard();

    let mut rng_impl = TestRng;
    let rng = RandomSource::new(&mut rng_impl);

    // A publishes / answers queries; B subscribes / queries.
    let res_a: SessionResources = SessionResources::new();
    let res_b: SessionResources = SessionResources::new();
    let pool_a: SubscriberPool = SubscriberPool::new();
    let pool_b: SubscriberPool = SubscriberPool::new();

    let mut pipe1: PipeLink = PipeLink::new();
    let mut pipe2: PipeLink = PipeLink::new();
    let (pipe1_read, pipe1_write) = pipe1.split();
    let (pipe2_read, pipe2_write) = pipe2.split();

    block_on(async {
        let connect = Network::connect(
            PipeLinkReceive::new(PipeRead::new(pipe1_read), 100),
            PipeLinkSend::new(PipeWrite::new(pipe2_write), 100),
            Duration::from_secs(30),
            rng.clone(),
            ZenohIdProto::rand(),
        );
        let accept = Network::accept(
            PipeLinkReceive::new(PipeRead::new(pipe2_read), 100),
            PipeLinkSend::new(PipeWrite::new(pipe1_write), 100),
            Duration::from_secs(30),
            rng,
            ZenohIdProto::rand(),
        );
        let (net_a, net_b) = join(connect, accept).await;
        let net_a = net_a.unwrap();
        let net_b = net_b.unwrap();

        let (sess_a, mut run_a) = Session::new(&res_a, &pool_a);
        let (sess_b, mut run_b) = Session::new(&res_b, &pool_b);

        // Keep both session transports pumping for the duration of the test logic.
        let run_a_f = async { run_a.run(net_a).await.expect("session runner A failed") };
        let run_b_f = async { run_b.run(net_b).await.expect("session runner B failed") };
        let runners = join(run_a_f, run_b_f);

        let a_side = async {
            let mut publisher = Publisher::declare(sess_a, "foo/1").await.unwrap();
            let mut queryable = Queryable::declare(sess_a, "q/x").await.unwrap();

            // Give B a moment to register its subscribers before publishing.
            Timer::after(Duration::from_millis(300)).await;
            publisher.publish(vec![1u8, 2, 3].into()).await.unwrap();

            // Answer exactly one query.
            let query = queryable.wait_for_query().await.unwrap();
            queryable
                .reply_to_query(query, Ok(b"pong".to_vec().into()))
                .await
                .unwrap();
        };

        let b_side = async {
            let mut sub_wild = Subscriber::declare(sess_b, "foo/*").await.unwrap();
            let mut sub_exact = Subscriber::declare(sess_b, "foo/1").await.unwrap();
            let mut sub_other = Subscriber::declare(sess_b, "bar").await.unwrap();

            // Wildcard subscription reports the concrete key that fired.
            let (key, payload) = with_timeout(Duration::from_secs(5), sub_wild.receive_keyed())
                .await
                .expect("timed out waiting for wildcard push")
                .unwrap();
            assert_eq!(key, "foo/1");
            assert_eq!(bytes(&payload), vec![1u8, 2, 3]);

            // An overlapping subscription on the same key also receives it.
            let payload = with_timeout(Duration::from_secs(5), sub_exact.receive())
                .await
                .expect("timed out waiting for overlapping push")
                .unwrap();
            assert_eq!(bytes(&payload), vec![1u8, 2, 3]);

            // A non-matching subscription receives nothing (times out).
            assert!(
                with_timeout(Duration::from_millis(500), sub_other.receive())
                    .await
                    .is_err(),
                "non-matching subscriber unexpectedly received a message"
            );

            // Query routing: the get is matched to the queryable and its reply comes back to us.
            let result = Get::new(sess_b, "q/x")
                .timeout(StdDuration::from_secs(5))
                .await
                .unwrap();
            match result {
                GetResult::Ok(payload) => assert_eq!(bytes(&payload), b"pong".to_vec()),
                GetResult::Err(_) => panic!("query returned an error reply"),
                GetResult::Timeout => panic!("query timed out"),
                GetResult::NoReply => panic!("query completed with no reply"),
            }

            // Pool exhaustion: declaring past the pool size yields NoSubscribeCapacity.
            let mut held = Vec::new();
            let mut exhausted = false;
            for _ in 0..64 {
                match Subscriber::declare(sess_b, "spam").await {
                    Ok(sub) => held.push(sub),
                    Err(SessionError::NoDispatcherCapacity) => {
                        exhausted = true;
                        break;
                    }
                    Err(other) => panic!("unexpected error declaring subscriber: {other:?}"),
                }
            }
            assert!(exhausted, "subscriber pool never reported exhaustion");
        };

        // The test logic finishes; the runners loop forever, so select drops them once we're done.
        match select(join(a_side, b_side), runners).await {
            Either::First(((), ())) => {}
            Either::Second(_) => panic!("a session runner exited before the test finished"),
        }
    });
}

/// The re-declaration set the session runner drains on (re)connect is built from the routing table
/// by `collect_redeclares`. Assert it covers every registered subscriber and queryable (and shrinks
/// when a consumer is dropped). No transport is needed, so this is fully deterministic.
#[test]
fn collect_redeclares_covers_registered_consumers() {
    let res: SessionResources = SessionResources::new();
    let pool: SubscriberPool = SubscriberPool::new();

    block_on(async {
        let (sess, _runner) = Session::new(&res, &pool);

        let sub_a = Subscriber::declare(sess, "a/*").await.unwrap();
        let _sub_b = Subscriber::declare(sess, "b").await.unwrap();
        let _queryable = Queryable::declare(sess, "q/x").await.unwrap();

        // Two subscribers + one queryable must all be re-declared; the get path is not persistent
        // and never appears here.
        let mut redeclares = Vec::new();
        pool.collect_redeclares(&mut redeclares);
        assert_eq!(redeclares.len(), 3, "expected 2 subscribers + 1 queryable");

        // Dropping a subscriber frees its slot and removes it from the re-declare set.
        drop(sub_a);
        let mut redeclares = Vec::new();
        pool.collect_redeclares(&mut redeclares);
        assert_eq!(redeclares.len(), 2);
    });
}

/// A single-shot `Get` (`.await`) must not hang forever if no response (or a dropped `Final`) ever
/// arrives — it bounds its wait with the configured timeout and returns an empty `Ok`. No responder
/// is registered here, so the get can only complete via the local timeout backstop. No transport is
/// needed (the request is just queued), so this is fully deterministic.
#[test]
fn single_shot_get_times_out_without_a_responder() {
    let res: SessionResources = SessionResources::new();
    let pool: SubscriberPool = SubscriberPool::new();

    block_on(async {
        let (sess, _runner) = Session::new(&res, &pool);

        let result = Get::new(sess, "nowhere")
            .timeout(StdDuration::from_millis(200))
            .await
            .unwrap();

        match result {
            // Nothing answers, so the local timeout fires. That is a timeout,
            // not a successful empty payload — conflating the two is what let a
            // dead peer read as "queried fine, found nothing".
            GetResult::Timeout => {}
            GetResult::Ok(_) => panic!("a get with no responder must not report a reply"),
            GetResult::Err(_) => panic!("unexpected error reply for a get with no responder"),
            GetResult::NoReply => panic!("expected a timeout, not a final without a reply"),
        }
    });
}

/// A subscription must survive a transport reconnect: the session runner re-declares every
/// registered consumer at the top of each `run()`. This drives B's session over two independent
/// transports (an original connection and a reconnect) with the subscriber declared *before* the
/// first `run()`, so the re-declare drain is non-empty on both rounds. It guards against the
/// reconnect re-declare deadlock and against losing a slot across sessions.
#[test]
fn subscription_survives_reconnect() {
    let _ = env_logger::builder().is_test(true).try_init();
    let _rng_guard = rng_guard();

    let mut rng_impl = TestRng;
    let rng = RandomSource::new(&mut rng_impl);

    // B is the long-lived side; its resources/pool/runner persist across both connections.
    let res_b: SessionResources = SessionResources::new();
    let pool_b: SubscriberPool = SubscriberPool::new();
    let (sess_b, mut run_b) = Session::new(&res_b, &pool_b);

    block_on(async {
        // Declared before any transport is up, so every run()'s re-declare drain is non-empty.
        let mut sub = Subscriber::declare(sess_b, "foo/*").await.unwrap();

        for round in 0u8..2 {
            // Fresh peer A and fresh pipes each round, as if the router restarted.
            let res_a: SessionResources = SessionResources::new();
            let pool_a: SubscriberPool = SubscriberPool::new();
            let mut pipe1: PipeLink = PipeLink::new();
            let mut pipe2: PipeLink = PipeLink::new();
            let (p1r, p1w) = pipe1.split();
            let (p2r, p2w) = pipe2.split();

            let connect = Network::connect(
                PipeLinkReceive::new(PipeRead::new(p1r), 100),
                PipeLinkSend::new(PipeWrite::new(p2w), 100),
                Duration::from_secs(30),
                rng.clone(),
                ZenohIdProto::rand(),
            );
            let accept = Network::accept(
                PipeLinkReceive::new(PipeRead::new(p2r), 100),
                PipeLinkSend::new(PipeWrite::new(p1w), 100),
                Duration::from_secs(30),
                rng.clone(),
                ZenohIdProto::rand(),
            );
            let (net_a, net_b) = join(connect, accept).await;
            let net_a = net_a.unwrap();
            let net_b = net_b.unwrap();

            let (sess_a, mut run_a) = Session::new(&res_a, &pool_a);

            let round_logic = async {
                let mut publisher = Publisher::declare(sess_a, "foo/1").await.unwrap();
                // Let B's session (re)establish and re-declare before publishing.
                Timer::after(Duration::from_millis(300)).await;
                publisher.publish(vec![round].into()).await.unwrap();

                let payload = with_timeout(Duration::from_secs(5), sub.receive())
                    .await
                    .expect("subscription did not survive reconnect")
                    .unwrap();
                assert_eq!(
                    bytes(&payload),
                    vec![round],
                    "wrong payload in round {round}"
                );
            };

            // Dropping the runner futures at the end of the round releases the &mut borrow on
            // `run_b`, so it can be re-run on the next round (the reconnect).
            match select(round_logic, join(run_a.run(net_a), run_b.run(net_b))).await {
                Either::First(()) => {}
                Either::Second(_) => panic!("a session runner exited early in round {round}"),
            }
        }
    });
}
