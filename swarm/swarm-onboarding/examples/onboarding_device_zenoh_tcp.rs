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

#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

extern crate alloc;

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_nal::{TcpConnect, TcpSplit, UdpBind, UdpReceive, UdpSend, UdpSplit};
use edge_nal_std::TcpSocket;

use embassy_executor::{Executor, Spawner};
use embassy_time::Duration;

use log::info;

use static_cell::StaticCell;

use swarm_onboarding::io::{SliceConsumer, SliceProducer};
use swarm_onboarding::qr::{Qr, QrPayload, QrTextType};
use swarm_onboarding::zenoh::device::DeviceOnboarding;
use swarm_onboarding::{DeviceError, DeviceKeys, DeviceProfile, OpNetFlags};

use x509_cert::Certificate;
use x509_cert::der::Decode;

use zenoh_nano::buffers::ZSlice;
use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::{LinkError, StreamingLinkReceive, StreamingLinkSend};
use zenoh_nano::network::Network;
use zenoh_nano::rng::{RandomSource, RngCore};
use zenoh_nano::scout::{
    SCOUT_BROADCAST_IP_ADDR, SCOUT_BROADCAST_PORT, SCOUT_MTU, ScoutLinkReceive, ScoutLinkSend,
    WhatAmIMatcher, ZenohIdProto,
};
use zenoh_nano::session::{Session, SessionResources, SessionRunner};

use zenoh_traits::nano::ZNSession;
use zenoh_traits::{Error, ErrorKind};

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

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

fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_nanos()
        .init();

    let our_ip = std::env::args()
        .nth(1)
        .map(|ip_str| ip_str.parse::<Ipv4Addr>().expect("Invalid IP address"));

    let executor = EXECUTOR.init(Executor::new());
    executor.run(move |spawner: Spawner| {
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

    let socket = mk_static!(TcpSocket, stack.connect(peer_addr).await.unwrap());

    let (read, write) = socket.split();

    // Initialize network

    let network = Network::connect(
        StreamingLinkReceive::new(read, 100),
        StreamingLinkSend::new(write, 100),
        Duration::from_secs(30),
        RandomSource::new(mk_static!(LocalRng, LocalRng)),
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

/// Device task:
/// - Initiate the onboarding process by communicating out-of-band
///   the Device Credentials and Profile to the Installer;
/// - Then listen for the onboarding meta-data and bundle, download and process those.
async fn device(session: ZNSession<'_>, device_keys: &DeviceKeys<'_>, buf: &mut [u8]) {
    info!("Running device...");

    let (dbuf, buf) = buf.split_at_mut(4096);

    let result: Result<(), DeviceError<ErrorKind, ErrorKind>> = async {
        let mut device = DeviceOnboarding::new(
            session,
            SliceProducer::new(CERTIFICATE),
            SliceConsumer::new(dbuf),
        );

        device
            .onboard(
                device_keys,
                &mut rand_08::thread_rng(),
                &mut rand_08::thread_rng(),
                buf,
            )
            .await?;

        Ok(())
    }
    .await;

    result.unwrap();
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
