//! A very simple ping-pong example where one peer periodically sends pings over a `ping` topic,
//! while the other receives the ping and replies with a ping on the `pong` topic
//!
//! A variation of the `ping_pong` example using TCP sockets rather than Embassy in-memory pipes as transport.

#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

extern crate alloc;

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_nal::{TcpAccept, TcpBind, TcpConnect, TcpSplit};
use edge_nal_std::TcpSocket;

use embassy_executor::{Executor, Spawner};
use embassy_futures::join::join;
use embassy_time::{Duration, Timer};

use log::info;

use static_cell::StaticCell;

use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::{StreamingLinkReceive, StreamingLinkSend};
use zenoh_nano::network::Network;
use zenoh_nano::ops::publish::Publisher;
use zenoh_nano::ops::subscribe::Subscriber;
use zenoh_nano::rng::{RandomSource, RngCore};
use zenoh_nano::scout::ZenohIdProto;
use zenoh_nano::session::{Session, SessionResources, SessionRunner};

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

    // For embedded targets, this should be the stack from `edge-nal-embassy`
    let stack = edge_nal_std::Stack::new();

    let acceptor = stack
        .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 4567))
        .await
        .unwrap();

    let (accept, connect) = join(
        acceptor.accept(),
        stack.connect(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4567)),
    )
    .await;

    let (_, ping_socket) = accept.unwrap();
    let ping_socket = mk_static!(TcpSocket, ping_socket);

    let (ping_read, ping_write) = ping_socket.split();

    let pong_socket = connect.unwrap();
    let pong_socket = mk_static!(TcpSocket, pong_socket);

    let (pong_read, pong_write) = pong_socket.split();

    // Initialize networks

    let rng = RandomSource::new(mk_static!(LocalRng, LocalRng));

    let ping_connect = Network::connect(
        StreamingLinkReceive::new(ping_read, 100),
        StreamingLinkSend::new(ping_write, 100),
        Duration::from_secs(30),
        rng.clone(),
        ZenohIdProto::rand(),
    );

    let pong_accept = Network::accept(
        StreamingLinkReceive::new(pong_read, 100),
        StreamingLinkSend::new(pong_write, 100),
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

    spawner.spawn(ping(ping_s).unwrap());
    spawner.spawn(pong(pong_s).unwrap());
}

/// Ping task: periodically sends pings and waits for pongs
#[embassy_executor::task]
async fn ping(session: Session<'static>) {
    let mut publisher = Publisher::declare(session, PING_TOPIC).await.unwrap();
    let mut subscriber = Subscriber::declare(session, PONG_TOPIC).await.unwrap();

    let mut payload: u32 = 0;

    loop {
        payload += 1;

        publisher
            .publish(payload.to_le_bytes().into())
            .await
            .unwrap();
        info!("Published ping: {}", payload);

        let data = subscriber.receive().await.unwrap();

        payload = u32::from_le_bytes(data.to_zslice().as_slice().try_into().unwrap());
        info!("Received pong: {}", payload);

        Timer::after(Duration::from_secs(5)).await;
    }
}

/// Pong task: waits for pings and replies with pongs
#[embassy_executor::task]
async fn pong(session: Session<'static>) {
    let mut publisher = Publisher::declare(session, PONG_TOPIC).await.unwrap();
    let mut subscriber = Subscriber::declare(session, PING_TOPIC).await.unwrap();

    loop {
        let data = subscriber.receive().await.unwrap();

        let mut payload = u32::from_le_bytes(data.to_zslice().as_slice().try_into().unwrap());
        info!("Received ping: {}", payload);

        payload += 1;

        publisher
            .publish(payload.to_le_bytes().into())
            .await
            .unwrap();
        info!("Published pong: {}", payload);
    }
}

/// Run the transport of a Zenoh session
#[embassy_executor::task(pool_size = 2)]
async fn run_session(
    mut runner: SessionRunner<'static>,
    network: Network<
        'static,
        StreamingLinkReceive<&'static TcpSocket>,
        StreamingLinkSend<&'static TcpSocket>,
    >,
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
