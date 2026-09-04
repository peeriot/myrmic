//! A very simple pin-pong example where one peer periodically sends pings over a `ping` topic,
//! while the other receives the ping and replies with a ping on the `pong` topic

#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

extern crate alloc;

use embassy_futures::join::join;
use log::info;

use embassy_executor::{Executor, Spawner};
use embassy_time::{Duration, Timer};

use static_cell::StaticCell;

use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::{PipeLink, PipeLinkReceive, PipeLinkSend, PipeRead, PipeWrite};
use zenoh_nano::network::Network;
use zenoh_nano::rng::{RandomSource, RngCore};
use zenoh_nano::scout::ZenohIdProto;
use zenoh_nano::session::{Session, SessionResources, SessionRunner};

use zenoh_traits::nano::{ZNError, ZNSession};
use zenoh_traits::{Close, Read as _, Receiver, SendPayload, Sender, Session as _, Write};

// Topics
const PING_TOPIC: &str = "ping";
const PONG_TOPIC: &str = "pong";

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .format_timestamp_nanos()
        .init();

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner: Spawner| {
        spawner.spawn(main_task(spawner).unwrap());
    });
}

macro_rules! mk_static {
    ($t:ty) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit();
        x
    }};
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write($val);
        x
    }};
}

/// Main task
#[embassy_executor::task]
async fn main_task(spawner: Spawner) {
    info!("Starting...");

    // Initialize links

    let pipe1 = mk_static!(PipeLink, PipeLink::new());
    let pipe2 = mk_static!(PipeLink, PipeLink::new());

    let (pipe1_read, pipe1_write) = pipe1.split();
    let (pipe2_read, pipe2_write) = pipe2.split();

    // Initialize networks

    let rng = RandomSource::new(mk_static!(LocalRng, LocalRng));

    let ping_connect = Network::connect(
        PipeLinkReceive::new(PipeRead::new(pipe1_read), 100),
        PipeLinkSend::new(PipeWrite::new(pipe2_write), 100),
        Duration::from_secs(30),
        rng.clone(),
        ZenohIdProto::rand(),
    );

    let pong_accept = Network::accept(
        PipeLinkReceive::new(PipeRead::new(pipe2_read), 100),
        PipeLinkSend::new(PipeWrite::new(pipe1_write), 100),
        Duration::from_secs(30),
        rng,
        ZenohIdProto::rand(),
    );

    // Connect the networks together
    // Use `join` to run both futures concurrently and to await until both sides are connected
    let (ping_network, pong_network) = join(ping_connect, pong_accept).await;

    let ping_network = ping_network.unwrap();
    let pong_network = pong_network.unwrap();

    // Initialize sessions' resources

    let ping_res = mk_static!(SessionResources, SessionResources::new());
    let ping_pool = mk_static!(SubscriberPool, SubscriberPool::new());
    let pong_res = mk_static!(SessionResources, SessionResources::new());
    let pong_pool = mk_static!(SubscriberPool, SubscriberPool::new());

    // Create and run the sessions

    let (ping_s, ping_r) = Session::new(ping_res, ping_pool);
    let (pong_s, pong_r) = Session::new(pong_res, pong_pool);

    spawner.spawn(run_session(ping_r, ping_network).unwrap());
    spawner.spawn(run_session(pong_r, pong_network).unwrap());

    // Run ping-pong tasks

    spawner.spawn(ping(ZNSession::new(ping_s)).unwrap());
    spawner.spawn(pong(ZNSession::new(pong_s)).unwrap());
}

/// Ping task: periodically sends pings and waits for pongs
#[embassy_executor::task]
async fn ping(session: ZNSession<'static>) {
    let result: Result<(), ZNError> = async {
        let mut publisher = session.publish(PING_TOPIC).await?;
        let mut subscriber = session.subscribe(PONG_TOPIC).await?;

        let mut payload: u32 = 0;

        loop {
            payload += 1;

            let mut write = publisher.send().await?.with_encoding("").await?;

            write.write_all(&payload.to_le_bytes()).await?;

            write.close().await?;

            info!("Published ping: {}", payload);

            let (_, mut read) = subscriber.receive().await?;

            let mut data = [0; 4];
            read.read_exact(&mut data).await.unwrap();

            read.close().await?;

            payload = u32::from_le_bytes(data);
            info!("Received pong: {}", payload);

            Timer::after(Duration::from_secs(5)).await;
        }
    }
    .await;

    result.unwrap();
}

/// Pong task: waits for pings and replies with pongs
#[embassy_executor::task]
async fn pong(session: ZNSession<'static>) {
    let result: Result<(), ZNError> = async {
        let mut publisher = session.publish(PONG_TOPIC).await?;
        let mut subscriber = session.subscribe(PING_TOPIC).await?;

        loop {
            let (_, mut read) = subscriber.receive().await?;

            let mut data = [0; 4];
            read.read_exact(&mut data).await.unwrap();

            read.close().await?;
            let mut payload = u32::from_le_bytes(data);
            info!("Received ping: {}", payload);

            payload += 1;

            let mut write = publisher.send().await?.with_encoding("").await?;

            write.write_all(&payload.to_le_bytes()).await?;

            write.close().await?;

            info!("Published pong: {}", payload);
        }
    }
    .await;

    result.unwrap();
}

/// Run the transport of a Zenoh session
#[embassy_executor::task(pool_size = 2)]
async fn run_session(
    mut runner: SessionRunner<'static>,
    network: Network<'static, PipeLinkReceive<'static>, PipeLinkSend<'static>>,
) {
    runner.run(network).await.unwrap()
}

struct LocalRng;

impl RngCore for LocalRng {
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
