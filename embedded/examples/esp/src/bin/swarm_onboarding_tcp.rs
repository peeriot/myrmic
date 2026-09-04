//! Example of a device onboarding over Zenoh (with `zenoh-nano`) and TCP.
//!
//! The example models the "device" peer in the onboarding process.
//!
//! In the real world, this would be useful for MCU's which are already connected to the IP network via an Ethernet cable.
//! For MCUs supporting Wifi (and BLE) - rather than using Ethernet - the BLE protocol would be more appropriate as the link
//! layer during onboarding.
//!
//! To run the example, first start `swarm` with:
//! ```sh
//! cargo run --features test-onboarding-plugin --bin swarm ./config/test_onboarding.jsonnet
//! ```
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

use esp_alloc::heap_allocator;
use esp_backtrace as _;
use esp_hal::ram;
use esp_hal::rng::{Rng, Trng};
use esp_hal::timer::timg::TimerGroup;
use esp_metadata_generated::memory_range;
use esp_radio::wifi::AuthenticationMethodConfig;
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, ControllerConfig, Interface, WifiController};

use log::info;

use swarm_onboarding::io::{SliceConsumer, SliceProducer};
use swarm_onboarding::qr::{Qr, QrPayload, QrTextType};
use swarm_onboarding::zenoh::device::DeviceOnboarding;
use swarm_onboarding::{DeviceKeys, DeviceProfile, OpNetFlags};

use x509_cert::Certificate;
use x509_cert::der::Decode;

use zenoh_nano::buffers::ZSlice;
use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::{LinkError, StreamingLinkReceive, StreamingLinkSend};
use zenoh_nano::network::Network;
use zenoh_nano::rng::RandomSource;
use zenoh_nano::scout::ZenohIdProto;
use zenoh_nano::scout::{
    SCOUT_BROADCAST_IP_ADDR, SCOUT_BROADCAST_PORT, SCOUT_MTU, ScoutLinkReceive, ScoutLinkSend,
    WhatAmIMatcher,
};
use zenoh_nano::session::{Session, SessionResources, SessionRunner};

use zenoh_traits::Error;
use zenoh_traits::nano::ZNSession;

extern crate alloc;

/// These are generated using the following command line:
/// ```sh
/// openssl req -x509 -newkey ec:<(openssl ecparam -name prime256v1) -nodes -days 3000 -keyout onboarding_private.pem -out onboarding_cert.der -outform DER -subj "/C=DE/ST=/L=Munich/O=Peeriot Ltd/OU=Com/CN=DEVICE-ID:123456"
/// openssl ec -in onboarding_private.pem -outform DER -out onboarding_private.der
/// ```
///
/// To view the cert & key:
/// ```sh
/// openssl x509 -inform der -in onboarding_cert.der -text -noout
/// openssl ec -inform der -in onboarding_private.der -text -noout
/// ```
const CERTIFICATE: &[u8] = include_bytes!("onboarding_cert.der");
const PRIVATE_KEY: &[u8] = include_bytes!("onboarding_private.der");

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

    let controller =
        esp_radio::wifi::WifiController::new(peripherals.WIFI, ControllerConfig::default())
            .unwrap();

    info!("Initialization complete.");

    let config = embassy_net::Config::dhcpv4(Default::default());

    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    // Init network stack
    let (stack, runner) = embassy_net::new(
        esp_radio::wifi::Interface::station(),
        config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

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

    let socket = mk_static!(TcpSocket, tcp_stack.connect(peer_addr).await.unwrap());

    let (read, write) = socket.split();

    // Initialize network

    let rng = mk_static!(esp_hal::rng::Rng, esp_hal::rng::Rng::new());

    let rng = RandomSource::new(rng);

    let network = Network::connect(
        StreamingLinkReceive::new(read, 100),
        StreamingLinkSend::new(write, 100),
        Duration::from_secs(30),
        rng,
        ZenohIdProto::rand(),
    )
    .await
    .unwrap();

    // Initialize session resources

    let resources = mk_static!(SessionResources, SessionResources::new());
    let pool = mk_static!(SubscriberPool, SubscriberPool::new());

    // Create and run the session

    let (session, runner) = Session::new(resources, pool);

    spawner.spawn(run_session(runner, network).unwrap());

    // Sample device credentials and profile

    // let device_keys = mk_static!(DeviceKeys, DeviceKeys::Insecure {
    //     device_id: b"example-device",
    // });

    let cert = mk_static!(Certificate, Certificate::from_der(CERTIFICATE).unwrap());
    let device_keys = mk_static!(
        DeviceKeys<'static>,
        DeviceKeys::PKI {
            pub_key: cert
                .tbs_certificate
                .subject_public_key_info
                .subject_public_key
                .as_bytes()
                .unwrap(),
            priv_key: PRIVATE_KEY
        }
    );

    let device_profile = DeviceProfile::new(OpNetFlags::all());

    let device_buf = mk_static!([u8; 30000], [0u8; 30000]);
    let creds = device_keys.creds();

    {
        let (device_profile_data, device_buf) = device_profile.serialize(device_buf).unwrap();

        let qr_payload = QrPayload::new(creds.pub_key().unwrap(), device_profile_data);
        let (qr_text, buf) = qr_payload.as_str(device_buf).unwrap();

        info!("QR-Text: {}", qr_text);

        let (tmp_buf, out_buf) = buf.split_at_mut(buf.len() / 2);

        let qr = Qr::compute(qr_text, tmp_buf, out_buf).unwrap();

        for line in qr.lines_range(QrTextType::Unicode, 4) {
            let (str, _) = qr
                .line_as_str(QrTextType::Unicode, 4, false, false, line, tmp_buf)
                .unwrap();

            info!("{}", str);
        }
    }

    // Run the device onboarding
    device(ZNSession::new(session), device_keys, device_buf).await;
}

/// Establishes and keeps a WiFi connection
#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    info!("Start Wifi connection task");

    let station_config = Config::Station(
        StationConfig::default()
            .with_ssid(WIFI_SSID.try_into().unwrap())
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                WIFI_PASS.try_into().unwrap(),
            )),
    );
    controller.set_config(&station_config).unwrap();

    let scan_config = ScanConfig::default().with_max(10);
    let result = controller.scan_async(&scan_config).await.unwrap();
    for ap in result {
        info!("{:?}", ap);
    }

    loop {
        if controller.is_connected() {
            let info = controller.wait_for_disconnect_async().await.ok();
            info!("Disconnected: {:?}", info);
            Timer::after(Duration::from_millis(5000)).await;
        }

        info!("About to connect...");

        match controller.connect_async().await {
            Ok(info) => info!("Wifi connected to {:?}", info),
            Err(e) => {
                info!("Failed to connect to wifi: {e:?}, retrying in 5 seconds...");
                Timer::after(Duration::from_millis(5000)).await;
            }
        }
    }
}

/// A task that runs the IP stack
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
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

/// Device task:
/// - Initiate the onboarding process by communicating out-of-band
///   the Device Credentials and Profile to the Installer;
/// - Then listen for the onboarding meta-data and bundle, download and process those.
async fn device(session: ZNSession<'_>, device_keys: &DeviceKeys<'_>, buf: &mut [u8]) {
    info!("Running device...");

    let (dbuf, buf) = buf.split_at_mut(4096);

    let mut consumer = SliceConsumer::new(dbuf);

    let mut device = DeviceOnboarding::new(session, SliceProducer::new(CERTIFICATE), &mut consumer);

    let mut trng1 = Trng::try_new().unwrap();
    let mut trng2 = Trng::try_new().unwrap();

    device
        .onboard(device_keys, &mut trng1, &mut trng2, buf)
        .await
        .unwrap();

    info!("============= Onboarding data received =============");
    info!("{}", core::str::from_utf8(consumer.data()).unwrap());
    info!("=====================================================");
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
