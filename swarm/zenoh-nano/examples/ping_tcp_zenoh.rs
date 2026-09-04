//! A very simple ping-pong example where one peer periodically sends pings over a `test/ping` topic,
//! while the other receives the ping and replies with a ping on the `test/pong` topic
//!
//! A variation of the `ping_pong_tcp` example using TCP sockets, where the `pong` peer is using the `zenoh` crate,
//! while we (`ping`) are using the `zenoh-nano` crate.
//!
//! Run the Zenoh `pong` peer first using:
//! ```sh
//! cd zenoh
//! cargo run --example z_pong -l tcp/0.0.0.0:7667
//! ```
//!
//! Scouting:
//! The example also allows for finding the Zenoh `pong` peer using scouting messages.
//! For this to work, provide the IP address of the machine running this example as the first argument:
//! ```sh
//! cargo run --example ping_tcp_zenoh AAA.BBB.CCC.DDD
//! ```
//!
//! ... and then run the Zenoh `pong` peer without providing a locator, **on another machine** (or else UDP port 7666 will be busy):
//! ```sh
//! cd zenoh
//! cargo run --example z_pong
//! ```
#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

extern crate alloc;

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_nal::{TcpConnect, TcpSplit, UdpBind, UdpReceive, UdpSend, UdpSplit};
use edge_nal_std::TcpSocket;

use embassy_executor::{Executor, Spawner};
use embassy_time::{Duration, Timer};

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
use zenoh_nano::scout::ZenohIdProto;
use zenoh_nano::scout::{
    SCOUT_BROADCAST_IP_ADDR, SCOUT_BROADCAST_PORT, SCOUT_MTU, ScoutLinkReceive, ScoutLinkSend,
    WhatAmIMatcher,
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

    let peer_addr = if let Some(our_ip) = our_ip {
        // Run a scouting loop to discover the Zenoh peer if the IP of our machine was provided as an argument
        scout(stack, our_ip).await
    } else {
        // Assume the Zenoh peer is running at this socket address
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7667)
    };

    // Initialize link

    let ping_socket = mk_static!(TcpSocket, stack.connect(peer_addr).await.unwrap());

    let (ping_read, ping_write) = ping_socket.split();

    // Initialize network

    let ping_network = Network::connect(
        StreamingLinkReceive::new(ping_read, 100),
        StreamingLinkSend::new(ping_write, 100),
        Duration::from_secs(30),
        RandomSource::new(mk_static!(LocalRng, LocalRng)),
        ZenohIdProto::rand(),
    )
    .await
    .unwrap();

    // Initialize session resources

    let ping_res = mk_static!(SessionResources, SessionResources::new());
    let ping_pool = mk_static!(SubscriberPool, SubscriberPool::new());

    // Create and run the session

    let (ping_s, ping_r) = Session::new(ping_res, ping_pool);

    spawner.spawn(run_session(ping_r, ping_network).unwrap());

    // Run ping-pong tasks

    spawner.spawn(ping(ping_s).unwrap());
}

async fn scout(stack: &'static edge_nal_std::Stack, our_ip: Ipv4Addr) -> SocketAddr {
    info!("Scouting for Zenoh nodes...");

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

    let mut socket_addr = None;

    // Run the scouting responder
    zenoh_nano::scout::run(
        UdpScoutLink(receive),
        UdpScoutLink(send),
        WhatAmIMatcher::empty().peer().client().router(),
        None,
        |_, hello| {
            for locator in &hello.locators {
                if locator.protocol().as_ref() == "tcp" {
                    info!("Discovered Zenoh node at {}", locator);

                    socket_addr = locator.address().as_ref().parse::<SocketAddr>().ok();

                    if let Some(socket_addr) = socket_addr {
                        info!("Using Zenoh node at {}", socket_addr);
                        return true;
                    }
                }
            }

            false
        },
    )
    .await
    .unwrap();

    socket_addr.unwrap()
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
