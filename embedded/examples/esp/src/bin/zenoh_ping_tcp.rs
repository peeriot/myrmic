//! A very simple TCP-based ping-pong example for the ESP32 where one peer periodically sends pings over a `test/ping` topic,
//! while the other receives the ping and replies with a ping on the `test/pong` topic
//!
//! Run the Zenoh `pong` peer first using:
//! ```sh
//! cd zenoh
//! cargo run --example z_pong -- -l tcp/0.0.0.0:7667
//! ```
//!
//! Scouting:
//! The example uses scouting to find the Zenoh `pong` peer using scouting messages.
#![no_std]
#![no_main]
#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use alloc::vec;

use edge_nal::{MulticastV4, TcpConnect, TcpSplit, UdpBind, UdpReceive, UdpSend, UdpSplit};
use edge_nal_embassy::{
    Tcp, TcpBuffers, TcpSocket, TcpSocketRead, TcpSocketWrite, Udp, UdpBuffers,
};

use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
use embassy_time::{Duration, Timer};

use embedded_io_async::Error;

use esp_alloc::heap_allocator;
use esp_backtrace as _;
use esp_hal::ram;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_metadata_generated::memory_range;
use esp_radio::wifi::AuthenticationMethodConfig;
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, ControllerConfig, Interface, WifiController};
use log::info;
use static_cell::StaticCell;
use zenoh_nano::buffers::ZSlice;
use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::{LinkError, StreamingLinkReceive, StreamingLinkSend};
use zenoh_nano::network::Network;
use zenoh_nano::ops::publish::Publisher;
use zenoh_nano::ops::subscribe::Subscriber;
use zenoh_nano::rng::RandomSource;
use zenoh_nano::scout::ZenohIdProto;
use zenoh_nano::scout::{
    SCOUT_BROADCAST_IP_ADDR, SCOUT_BROADCAST_PORT, SCOUT_MTU, ScoutLinkReceive, ScoutLinkSend,
    WhatAmIMatcher,
};
use zenoh_nano::session::{Session, SessionResources, SessionRunner};

extern crate alloc;

macro_rules! mk_static {
    ($t:ty) => {{
        #[cfg(not(feature = "esp32"))]
        {
            static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
            STATIC_CELL.uninit()
        }
        #[cfg(feature = "esp32")]
        alloc::boxed::Box::leak(alloc::boxed::Box::<$t>::new_uninit())
    }};
    ($t:ty,$val:expr) => {{
        #[cfg(not(feature = "esp32"))]
        {
            static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
            #[deny(unused_attributes)]
            let x = STATIC_CELL.uninit().write($val);
            x
        }
        #[cfg(feature = "esp32")]
        alloc::boxed::Box::leak(alloc::boxed::Box::<$t>::new($val))
    }};
}

/// Backoff delay between WiFi reconnect attempts
const WIFI_RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

#[cfg(not(feature = "esp32"))]
const HEAP_SIZE: usize = 100 * 1024;
#[cfg(feature = "esp32")]
const HEAP_SIZE: usize = 140 * 1024;

const RECLAIMED_RAM: usize =
    memory_range!("DRAM2_UNINIT").end - memory_range!("DRAM2_UNINIT").start;

esp_bootloader_esp_idf::esp_app_desc!();

/// Set your Wifi SSID via the `WIFI_SSID` environment variable
const WIFI_SSID: &str = if let Some(wifi_ssid) = option_env!("WIFI_SSID") {
    wifi_ssid
} else {
    "test"
};

/// Set your Wifi password via the `WIFI_PASS` environment variable
const WIFI_PASS: &str = if let Some(wifi_ssid) = option_env!("WIFI_PASS") {
    wifi_ssid
} else {
    "test"
};

// Topics
const PING_TOPIC: &str = "test/ping";
const PONG_TOPIC: &str = "test/pong";

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger(log::LevelFilter::Info);

    info!("Starting...");

    // Configure the heap

    heap_allocator!(size: HEAP_SIZE - RECLAIMED_RAM);
    heap_allocator!(#[ram(reclaimed)] size: RECLAIMED_RAM);

    // Necessary `esp-hal` and `esp-radio` initialization boilerplate

    let peripherals = esp_hal::init(esp_hal::Config::default());

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    static STACK_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let stack_resources = STACK_RESOURCES.init(StackResources::new());

    let controller =
        esp_radio::wifi::WifiController::new(peripherals.WIFI, ControllerConfig::default())
            .unwrap();
    let wifi_interface = esp_radio::wifi::Interface::station();

    let config = embassy_net::Config::dhcpv4(Default::default());

    let rng = Rng::new();
    let seed = u64::from(rng.random()) << 32 | u64::from(rng.random());

    // Init network stack
    let (stack, runner) = embassy_net::new(wifi_interface, config, stack_resources, seed);

    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(runner).unwrap());

    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    info!("Waiting to get IP address...");

    let config = loop {
        if let Some(config) = stack.config_v4() {
            info!("Got IP: {}", config.address);
            break config;
        }
        Timer::after(Duration::from_millis(500)).await;
    };

    // Run a scouting loop to discover the Zenoh peer if the IP of our machine was provided as an argument

    let udp_buffers = &*mk_static!(UdpBuffers::<1>, UdpBuffers::new());
    let udp_stack = Udp::new(stack, udp_buffers);

    let peer_addr = scout(udp_stack, config.address.address()).await;

    // Initialize link

    let tcp_buffers = &*mk_static!(TcpBuffers::<1>, TcpBuffers::new());
    let tcp_stack = &*mk_static!(Tcp, Tcp::new(stack, tcp_buffers));

    let ping_socket = mk_static!(TcpSocket, tcp_stack.connect(peer_addr).await.unwrap());

    let (ping_read, ping_write) = ping_socket.split();

    // Initialize network

    let rng = mk_static!(esp_hal::rng::Rng, esp_hal::rng::Rng::new());

    let rng = RandomSource::new(rng);

    let ping_network = Network::connect(
        StreamingLinkReceive::new(ping_read, 100),
        StreamingLinkSend::new(ping_write, 100),
        Duration::from_secs(30),
        rng,
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

/// Establishes and keeps a WiFi connection
#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    log::info!("start connection task");

    let station_config = Config::Station(
        StationConfig::default()
            .with_ssid(WIFI_SSID.try_into().unwrap())
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                WIFI_PASS.try_into().unwrap(),
            )),
    );
    controller.set_config(&station_config).unwrap();

    log::info!("Scan");
    let scan_config = ScanConfig::default().with_max(10);
    let result = controller.scan_async(&scan_config).await.unwrap();
    for ap in result {
        log::info!("{:?}", ap);
    }

    loop {
        if controller.is_connected() {
            // wait until we're no longer connected
            let info = controller.wait_for_disconnect_async().await.ok();
            log::info!("Disconnected: {:?}", info);
            Timer::after(WIFI_RECONNECT_BACKOFF).await;
        }
        log::info!("About to connect...");

        match controller.connect_async().await {
            Ok(info) => log::info!("Wifi connected to {:?}", info),
            Err(e) => {
                log::error!("Failed to connect to wifi: {e:?}");
                Timer::after(WIFI_RECONNECT_BACKOFF).await;
            }
        }
    }
}

/// A task that runs the IP stack
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await;
}

/// Scouting function: discovers a Zenoh node using UDP scouting messages
async fn scout<S>(stack: S, our_ip: Ipv4Addr) -> SocketAddr
where
    S: UdpBind,
{
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

    let mut udp_socket = stack
        .bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            SCOUT_BROADCAST_PORT,
        ))
        .await
        .unwrap();

    udp_socket
        .join_v4(SCOUT_BROADCAST_IP_ADDR, our_ip)
        .await
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
                        if matches!(socket_addr.ip(), IpAddr::V4(..)) {
                            info!("Using Zenoh node at {}", socket_addr);
                            return true;
                        }
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
        StreamingLinkReceive<TcpSocketRead<'static>>,
        StreamingLinkSend<TcpSocketWrite<'static>>,
    >,
) {
    runner.run(network).await.unwrap()
}
