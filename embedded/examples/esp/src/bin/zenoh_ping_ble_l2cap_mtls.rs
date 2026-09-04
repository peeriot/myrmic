//! BLE mTLS + zenoh-nano example for ESP32-C6.
#![no_std]
#![no_main]
#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)]

extern crate alloc;

use examples_esp::{CaCheckingProvider, mk_static};

use bt_hci::controller::ExternalController;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_alloc::heap_allocator;
use esp_backtrace as _;
use esp_hal::{ram, timer::timg::TimerGroup};
use esp_metadata_generated::memory_range;
use esp_radio::ble::controller::BleConnector;
use log::info;
use trouble_host::{l2cap::L2capChannel, prelude::*};

use zenoh_nano::scout::ZenohIdProto;
use zenoh_nano::{
    dispatch::SubscriberPool,
    link::{
        l2cap::{L2capStream, SWARM_TLS_PSM, swarm_l2cap_config},
        tls::{MutualTlsConfig, TlsBuffers, TlsLinkChannels, mtls_connect_and_split},
        trouble::SERVICE_UUID,
    },
    network::Network,
    ops::get::Get,
    rng::RandomSource,
    session::{Session, SessionResources},
};

const CA_CERT: &[u8] = include_bytes!("../../../../../tests/integration/certs/ca.der");
const ESP_CERT: &[u8] = include_bytes!("../../../../../tests/integration/certs/esp.der");
const ESP_KEY: &[u8] = include_bytes!("../../../../../tests/integration/certs/esp.key.der");

#[cfg(not(feature = "esp32"))]
const HEAP_SIZE: usize = 100 * 1024;
#[cfg(feature = "esp32")]
const HEAP_SIZE: usize = 140 * 1024;

const RECLAIMED_RAM: usize =
    memory_range!("DRAM2_UNINIT").end - memory_range!("DRAM2_UNINIT").start;

esp_bootloader_esp_idf::esp_app_desc!();

const MTLS_QUERY_NODE_STATUS_V1: &str = "@introspection/@v1/@node-status";
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const BLE_ADV_INTERVAL_MIN: Duration = Duration::from_millis(200);
const BLE_ADV_INTERVAL_MAX: Duration = Duration::from_millis(400);
const PING_INTERVAL: Duration = Duration::from_secs(5);

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 3; // Signal + ATT + CoC

type PingHostResources = trouble_host::HostResources<
    PingCtrl<'static>,
    DefaultPacketPool,
    CONNECTIONS_MAX,
    L2CAP_CHANNELS_MAX,
>;
type PingCtrl<'d> = ExternalController<BleConnector<'d>, 20>;

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger(log::LevelFilter::Info);
    info!("[BOOT] zenoh_ping_ble_l2cap_mtls");

    heap_allocator!(size: HEAP_SIZE - RECLAIMED_RAM);
    heap_allocator!(#[ram(reclaimed)] size: RECLAIMED_RAM);

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);
    let controller = PingCtrl::new(BleConnector::new(peripherals.BT, Default::default()).unwrap());

    let host_res = mk_static!(PingHostResources, PingHostResources::new());
    let stack = mk_static!(
        Stack<'static, PingCtrl<'static>, DefaultPacketPool>,
        trouble_host::new(controller, host_res).build()
    );
    let rng = mk_static!(esp_hal::rng::Rng, esp_hal::rng::Rng::new());
    let rng_source = RandomSource::new(rng);

    let peripheral = stack.peripheral();
    let runner = stack.runner();

    spawner.spawn(ble_runner(runner).unwrap());

    info!("[BLE] Advertising for Linux pong to connect...");
    let (_conn, channel) = accept_ble_l2cap(stack, peripheral).await;
    info!("[L2CAP] Channel opened");

    let stream = L2capStream::new(channel, stack);

    let tls_bufs = mk_static!(TlsBuffers, TlsBuffers::new());
    let channels = mk_static!(TlsLinkChannels, TlsLinkChannels::new());

    let mtls = MutualTlsConfig {
        ca_certificate_der: CA_CERT,
        certificate_der: ESP_CERT,
        private_key_der: ESP_KEY,
        server_name: Some("laptop-test"),
        max_fragment_length: embedded_tls::MaxFragmentLength::Bits9,
    };
    let provider = CaCheckingProvider::new(rand_core::OsRng);

    info!("[TLS] Handshake started");

    let (tls_runner, tls_rx, tls_tx) = match embassy_time::with_timeout(
        TLS_HANDSHAKE_TIMEOUT,
        mtls_connect_and_split(stream, tls_bufs, mtls, channels, provider),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            log::error!("[TLS] Handshake error: {:?}", e);
            return;
        }
        Err(_) => {
            log::error!("[TLS] Handshake timeout");
            return;
        }
    };
    info!("[TLS] Handshake complete");

    spawner.spawn(run_tls_link(tls_runner).unwrap());

    let network = match Network::connect(
        tls_rx,
        tls_tx,
        NETWORK_CONNECT_TIMEOUT,
        rng_source,
        ZenohIdProto::rand(),
    )
    .await
    {
        Ok(n) => n,
        Err(e) => {
            log::error!("[ZENOH] Network connect failed: {:?}", e);
            return;
        }
    };

    let resources = mk_static!(SessionResources, SessionResources::new());
    let pool = mk_static!(SubscriberPool, SubscriberPool::new());
    let (session, mut session_runner) = Session::new(resources, pool);

    spawner.spawn(ping(session).unwrap());
    match session_runner.run(network).await {
        Ok(()) => info!("[ZENOH] Session ended"),
        Err(e) => log::error!("[ZENOH] Session error: {:?}", e),
    }
}

async fn accept_ble_l2cap(
    stack: &'static Stack<'static, PingCtrl<'static>, DefaultPacketPool>,
    mut peripheral: Peripheral<'static, PingCtrl<'static>, DefaultPacketPool>,
) -> (
    Connection<'static, DefaultPacketPool>,
    L2capChannel<'static, DefaultPacketPool>,
) {
    let service_uuids = &[SERVICE_UUID.to_le_bytes()];
    let adv_structs = [
        AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
        AdStructure::ServiceUuids128(service_uuids),
        AdStructure::CompleteLocalName(b"ZN"),
    ];
    let mut adv_buf = [0u8; 31];
    let len =
        AdStructure::encode_slice(&adv_structs, &mut adv_buf).expect("advertisement must fit");

    loop {
        info!("[BLE] Advertising...");
        let advertiser = peripheral
            .advertise(
                &AdvertisementParameters {
                    interval_min: BLE_ADV_INTERVAL_MIN,
                    interval_max: BLE_ADV_INTERVAL_MAX,
                    ..Default::default()
                },
                Advertisement::ConnectableScannableUndirected {
                    adv_data: &adv_buf[..len],
                    scan_data: &[],
                },
            )
            .await
            .unwrap();

        let conn = advertiser.accept().await.unwrap();
        info!("[BLE] Connection established");

        let config = swarm_l2cap_config();
        match L2capChannel::accept(stack, &conn, &[SWARM_TLS_PSM], &config).await {
            Ok(ch) => return (conn, ch),
            Err(e) => {
                log::warn!("[L2CAP] Accept failed: {:?}; re-advertising", e);
            }
        }
    }
}

#[embassy_executor::task]
async fn ble_runner(mut runner: Runner<'static, PingCtrl<'static>, DefaultPacketPool>) {
    runner.run().await.unwrap();
}

#[embassy_executor::task]
async fn run_tls_link(
    runner: zenoh_nano::link::tls::TlsLinkRunner<
        'static,
        L2capStream<'static, 'static, PingCtrl<'static>>,
    >,
) {
    if let Err(e) = runner.run().await {
        log::error!("[TLS] Runner error: {:?}", e);
    }
}

#[embassy_executor::task]
async fn ping(session: Session<'static>) {
    loop {
        match Get::new(session, MTLS_QUERY_NODE_STATUS_V1).await {
            Ok(_) => {
                info!("[ZENOH] node-status reply ok");
            }
            Err(e) => {
                log::error!("[ZENOH] get node-status failed: {:?}", e);
                return;
            }
        }
        Timer::after(PING_INTERVAL).await;
    }
}
