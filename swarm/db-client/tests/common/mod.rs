//! Two in-process `zenoh-nano` sessions wired together over in-memory pipes,
//! mirroring zenoh-nano's dispatch tests: one side plays a db node (declaring
//! whatever queryables the test needs), the other drives a `db_client` client.

use embassy_futures::block_on;
use embassy_futures::join::join;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, with_timeout};

use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::{PipeLink, PipeLinkReceive, PipeLinkSend, PipeRead, PipeWrite};
use zenoh_nano::network::Network;
use zenoh_nano::rng::{RandomSource, RngCore};
use zenoh_nano::session::{Session, SessionResources};
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

pub fn decode<T: serde::de::DeserializeOwned>(buf: &zenoh_nano::buffers::ZBuf) -> T {
    postcard::from_bytes(buf.to_zslice().as_slice()).expect("payload should decode")
}

pub fn encode<T: serde::Serialize>(value: &T) -> zenoh_nano::buffers::ZBuf {
    postcard::to_allocvec(value)
        .expect("payload should encode")
        .into()
}

/// How long a test body gets before it counts as wedged. A side waiting on a
/// message the other side never sends would otherwise hang forever, which is
/// exactly what a round-trip regression looks like from in here.
const BODY_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `body` against a linked pair of sessions: the node side first, the
/// client side second. Panics if the session runners end before `body` does,
/// or if `body` outlasts [`BODY_TIMEOUT`].
pub fn with_linked_sessions<F>(body: F)
where
    F: AsyncFnOnce(Session<'static>, Session<'static>),
{
    let _ = env_logger::builder().is_test(true).try_init();

    let mut rng_impl = TestRng;
    let rng = RandomSource::new(&mut rng_impl);

    // The `Client` requires a `Session<'static>`, so its halves must outlive
    // the test body.
    let res_a: &'static SessionResources = Box::leak(Box::new(SessionResources::new()));
    let res_b: &'static SessionResources = Box::leak(Box::new(SessionResources::new()));
    let pool_a: &'static SubscriberPool = Box::leak(Box::new(SubscriberPool::new()));
    let pool_b: &'static SubscriberPool = Box::leak(Box::new(SubscriberPool::new()));

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

        let (sess_a, mut run_a) = Session::new(res_a, pool_a);
        let (sess_b, mut run_b) = Session::new(res_b, pool_b);

        let run_a_f = async { run_a.run(net_a).await.expect("session runner A failed") };
        let run_b_f = async { run_b.run(net_b).await.expect("session runner B failed") };
        let runners = join(run_a_f, run_b_f);

        match select(runners, with_timeout(BODY_TIMEOUT, body(sess_a, sess_b))).await {
            Either::Second(Ok(())) => (),
            Either::Second(Err(_)) => panic!("the test body wedged waiting on the other side"),
            Either::First(_) => panic!("session runners ended early"),
        }
    });
}
