//! A very simple ping-pong example where one peer periodically sends pings over a `test/ping` topic,
//! while the other receives the ping and replies with a ping on the `test/pong` topic
//!
//! A variation of the `ping_pong_tcp` example using TCP sockets, where the `ping` peer is using the `zenoh` crate,
//! while we (`pong`) are using the `zenoh-nano` crate.
//!
//! Run this example first, and then the Zenoh `ping` peer second using:
//! ```sh
//! cd zenoh
//! cargo run --example z_ping -e tcp/0.0.0.0:7667 10
//! ```
//!
//! Scouting:
//! The example also allows for the Zenoh `ping` peer to find us using scouting messages.
//! For this to work, provide the IP address of the machine running this example as the first argument:
//! ```sh
//! cargo run --example pong_tcp_zenoh AAA.BBB.CCC.DDD
//! ```
//!
//! ... and then run the Zenoh `ping` peer without providing a locator, **on another machine** (or else UDP port 7666 will be busy):
//! ```sh
//! cd zenoh
//! cargo run --example z_ping 10
//! ```
#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

extern crate alloc;

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_nal::{TcpAccept, TcpBind, TcpSplit, UdpBind, UdpReceive, UdpSend, UdpSplit};
use edge_nal_std::TcpSocket;

use embassy_executor::{Executor, Spawner};
use embassy_time::Duration;

use embedded_io_async::Error;

use log::info;

use static_cell::StaticCell;

use zenoh_nano::buffers::ZSlice;
use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::{LinkError, StreamingLinkReceive, StreamingLinkSend};
use zenoh_nano::network::Network;
use zenoh_nano::ops::publish::Publisher;
use zenoh_nano::ops::subscribe::Subscriber;
use zenoh_nano::rng::{RandomSource, RngCore};
use zenoh_nano::scout::{
    SCOUT_BROADCAST_IP_ADDR, SCOUT_BROADCAST_PORT, SCOUT_MTU, ScoutLinkReceive, ScoutLinkSend,
    WhatAmI, WhatAmIMatcher, ZenohIdProto,
};
use zenoh_nano::session::{Session, SessionResources, SessionRunner};

// Topics
const PING_TOPIC: &str = "test/ping";
const PONG_TOPIC: &str = "test/pong";

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .format_timestamp_nanos()
        .init();

    let our_ip = std::env::args()
        .nth(1)
        .map(|ip_str| ip_str.parse::<Ipv4Addr>().expect("Invalid IP address"));

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner: Spawner| {
        spawner.spawn(main_task(spawner, our_ip).unwrap());
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
async fn main_task(spawner: Spawner, our_ip: Option<Ipv4Addr>) {
    info!("Starting...");

    // For embedded targets, this should be the stack from `edge-nal-embassy`
    let stack = mk_static!(edge_nal_std::Stack, edge_nal_std::Stack::new());

    // Run the scouting replies' loop if the IP of the machine was provided as an argument
    if let Some(our_ip) = our_ip {
        spawner.spawn(scout_reply(stack, our_ip).unwrap());
    }

    // Initialize link

    info!("Listening for Zenoh protocol messages on TCP port 7667...");

    let acceptor = TcpBind::bind(
        stack,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 7667),
    )
    .await
    .unwrap();

    let (_, pong_socket) = acceptor.accept().await.unwrap();

    let pong_socket = mk_static!(TcpSocket, pong_socket);

    let (pong_read, pong_write) = pong_socket.split();

    // Initialize network

    let pong_accept = Network::accept(
        StreamingLinkReceive::new(pong_read, 100),
        StreamingLinkSend::new(pong_write, 100),
        Duration::from_secs(30),
        RandomSource::new(mk_static!(LocalRng, LocalRng)),
        ZenohIdProto::rand(),
    )
    .await
    .unwrap();

    // Initialize session resources

    let pong_res = mk_static!(SessionResources, SessionResources::new());
    let pong_pool = mk_static!(SubscriberPool, SubscriberPool::new());

    // Create and run the session

    let (pong_s, pong_r) = Session::new(pong_res, pong_pool);

    spawner.spawn(run_session(pong_r, pong_accept).unwrap());

    // Run ping-pong tasks

    spawner.spawn(pong(pong_s).unwrap());
}

#[embassy_executor::task]
async fn scout_reply(stack: &'static edge_nal_std::Stack, our_ip: Ipv4Addr) {
    info!("Listening for Zenoh scout requests...");

    // Adapt the `edge-nal` UDP socket to the ScoutLink traits of `zenoh-nano`
    struct UdpScoutLink<T>(T);

    impl<T> ScoutLinkReceive for UdpScoutLink<T>
    where
        T: UdpReceive,
    {
        async fn receive(&mut self) -> Result<(SocketAddr, ZSlice), LinkError> {
            let mut buf = vec![0u8; SCOUT_MTU as usize];

            let (len, addr) = self
                .0
                .receive(&mut buf)
                .await
                .map_err(|e| LinkError::Io(e.kind()))?;

            buf.truncate(len);

            Ok((addr, ZSlice::from(buf)))
        }
    }

    impl<T> ScoutLinkSend for UdpScoutLink<T>
    where
        T: UdpSend,
    {
        async fn send(&mut self, addr: &SocketAddr, data: ZSlice) -> Result<(), LinkError> {
            self.0
                .send(*addr, data.as_slice())
                .await
                .map_err(|e| LinkError::Io(e.kind()))?;

            Ok(())
        }
    }

    let mut udp_socket = UdpBind::bind(
        stack,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), SCOUT_BROADCAST_PORT),
    )
    .await
    .unwrap();

    udp_socket
        .join_multicast_v4(&SCOUT_BROADCAST_IP_ADDR, &our_ip)
        .unwrap();

    let (receive, send) = udp_socket.split();

    // Run the scouting responder
    zenoh_nano::scout::run(
        UdpScoutLink(receive),
        UdpScoutLink(send),
        WhatAmIMatcher::empty(),
        Some(zenoh_protocol::scouting::HelloProto {
            version: zenoh_protocol::VERSION,
            whatami: WhatAmI::Peer,
            zid: ZenohIdProto::default(),
            locators: vec![format!("tcp/{our_ip}:7667").try_into().unwrap()],
        }),
        |_, _| false,
    )
    .await
    .unwrap();
}

/// Pong task: waits for pings and replies with pongs
#[embassy_executor::task]
async fn pong(session: Session<'static>) {
    let mut publisher = Publisher::declare(session, PONG_TOPIC).await.unwrap();
    let mut subscriber = Subscriber::declare(session, PING_TOPIC).await.unwrap();

    loop {
        let data = subscriber.receive().await.unwrap();

        let payload = data.to_zslice();
        info!("Received ping: {:?}", payload.as_slice());

        info!("About to publish pong: {:?}", payload.as_slice());

        publisher.publish(data).await.unwrap();
    }
}

/// Run the transport of a Zenoh session
#[embassy_executor::task]
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
