//! WiFi mTLS + zenoh-nano query example for ESP32.
//!
//! Flow:
//! 1. Join WiFi (STA)
//! 2. Open TCP to machine endpoint (`MTLS_WIFI_SERVER_ADDR`)
//! 3. Establish mTLS with embedded-tls
//! 4. Run zenoh-nano session over TLS link
//! 5. Query `@introspection/@v1/@node-status`
#![no_std]
#![no_main]
#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)]

use core::net::SocketAddr;

use edge_nal::TcpConnect;
use edge_nal_embassy::{Tcp, TcpBuffers, TcpSocket};
use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
use embassy_time::{Duration, Timer};
use esp_alloc::heap_allocator;
use esp_backtrace as _;
use esp_hal::{ram, rng::Rng, timer::timg::TimerGroup};
use esp_metadata_generated::memory_range;
use esp_radio::wifi::AuthenticationMethodConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config, ControllerConfig, Interface, WifiController};
use examples_esp::{CaCheckingProvider, mk_static};
use log::info;

use rand_core::OsRng;
use zenoh_nano::ops::get::{Get, GetResult};
use zenoh_nano::{
    dispatch::SubscriberPool,
    link::tls::{MutualTlsConfig, TlsBuffers, TlsLinkChannels, mtls_connect_and_split},
    network::Network,
    rng::RandomSource,
    session::{Session, SessionResources},
};

const CA_CERT: &[u8] = include_bytes!("../../../../../tests/integration/certs/ca.der");
const ESP_CERT: &[u8] = include_bytes!("../../../../../tests/integration/certs/esp.der");
const ESP_KEY: &[u8] = include_bytes!("../../../../../tests/integration/certs/esp.key.der");

const WIFI_SSID: &str = if let Some(wifi_ssid) = option_env!("WIFI_SSID") {
    wifi_ssid
} else {
    "UNCONFIGURED"
};

const WIFI_PASS: &str = if let Some(wifi_pass) = option_env!("WIFI_PASS") {
    wifi_pass
} else {
    "UNCONFIGURED"
};

const SERVER_ADDR: &str = if let Some(server_addr) = option_env!("MTLS_WIFI_SERVER_ADDR") {
    server_addr
} else {
    "UNCONFIGURED"
};

const BUILD_MARKER: &str = "MTLS_QUERY_NODE_STATUS_V1";
const NODE_STATUS_QUERY: &str = "@introspection/@v1/@node-status";
const WIFI_POLL_INTERVAL: Duration = Duration::from_millis(500);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const NETWORK_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const WIFI_RETRY_DELAY: Duration = Duration::from_millis(5000);
const QUERY_INTERVAL: Duration = Duration::from_secs(5);

#[cfg(not(feature = "esp32"))]
const HEAP_SIZE: usize = 100 * 1024;
#[cfg(feature = "esp32")]
const HEAP_SIZE: usize = 140 * 1024;

const RECLAIMED_RAM: usize =
    memory_range!("DRAM2_UNINIT").end - memory_range!("DRAM2_UNINIT").start;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger(log::LevelFilter::Info);
    info!("[BOOT] zenoh_ping_tcp_mtls");
    info!("[BOOT] firmware marker {}", BUILD_MARKER);

    heap_allocator!(size: HEAP_SIZE - RECLAIMED_RAM);
    heap_allocator!(#[ram(reclaimed)] size: RECLAIMED_RAM);

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);
    let (controller, interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, ControllerConfig::default()).unwrap();

    let config = embassy_net::Config::dhcpv4(Default::default());
    let rng_seed = Rng::new();
    let seed = (rng_seed.random() as u64) << 32 | rng_seed.random() as u64;

    let (stack, runner) = embassy_net::new(
        interfaces.station,
        config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

    spawner.spawn(connection(controller)).unwrap();
    spawner.spawn(net_task(runner)).unwrap();

    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(WIFI_POLL_INTERVAL).await;
    }
    loop {
        if let Some(cfg) = stack.config_v4() {
            info!("[WIFI] Got IP {}", cfg.address);
            break;
        }
        Timer::after(WIFI_POLL_INTERVAL).await;
    }

    let tcp_buffers = &*mk_static!(TcpBuffers::<1>, TcpBuffers::new());
    let tcp_stack = &*mk_static!(Tcp<'_>, Tcp::new(stack, tcp_buffers));

    let server_addr = match SERVER_ADDR.parse::<SocketAddr>() {
        Ok(v) => v,
        Err(_) => {
            log::error!("[FAIL] invalid server addr: {}", SERVER_ADDR);
            return;
        }
    };

    info!("[TCP] Connecting to {}", server_addr);
    let socket = match tcp_stack.connect(server_addr).await {
        Ok(s) => mk_static!(TcpSocket<'_>, s),
        Err(e) => {
            log::error!("[FAIL] tcp connect failed: {:?}", e);
            return;
        }
    };
    info!("[TCP] Connected");

    // Initialize the global getrandom provider before any TLS/zenoh path
    // touches rand_core::OsRng.
    let rng_hw = mk_static!(esp_hal::rng::Rng, esp_hal::rng::Rng::new());
    let rng_source = RandomSource::new(rng_hw);

    let tls_buffers = mk_static!(TlsBuffers, TlsBuffers::new());
    let tls_channels = mk_static!(TlsLinkChannels, TlsLinkChannels::new());

    let mtls = MutualTlsConfig {
        ca_certificate_der: CA_CERT,
        certificate_der: ESP_CERT,
        private_key_der: ESP_KEY,
        server_name: Some("laptop-test"),
        max_fragment_length: embedded_tls::MaxFragmentLength::Bits9,
    };
    // OsRng is backed by the getrandom hook installed by RandomSource::new above.
    // CaCheckingProvider must be constructed after that call.
    let provider = CaCheckingProvider::new(OsRng);

    info!("[TLS] Handshake started");
    let (tls_runner, tls_recv, tls_send) = match embassy_time::with_timeout(
        TLS_HANDSHAKE_TIMEOUT,
        mtls_connect_and_split(socket, tls_buffers, mtls, tls_channels, provider),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            log::error!("[FAIL] tls connect failed: {:?}", e);
            return;
        }
        Err(_) => {
            log::error!("[FAIL] tls connect timeout");
            return;
        }
    };
    info!("[TLS] Handshake complete");
    spawner.spawn(run_tls_link(tls_runner).unwrap());

    let network = match Network::connect(
        tls_recv,
        tls_send,
        NETWORK_CONNECT_TIMEOUT,
        rng_source.clone(),
        ZenohIdProto::rand(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            log::error!("[FAIL] zenoh network connect failed: {:?}", e);
            return;
        }
    };

    let resources = mk_static!(SessionResources, SessionResources::new());
    let pool = mk_static!(SubscriberPool, SubscriberPool::new());
    let (session, mut session_runner) = Session::new(resources, pool);

    spawner.spawn(query_node_status(session).unwrap());
    match session_runner.run(network).await {
        Ok(()) => info!("[ZENOH] Session runner finished"),
        Err(e) => log::error!("[FAIL] session runner error: {:?}", e),
    }
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    info!("[WIFI] Connection task started");
    loop {
        if matches!(controller.is_connected(), Ok(true)) {
            let info = controller.wait_for_disconnect_async().await.ok();
            info!("[WIFI] Disconnected: {:?}", info);
            Timer::after(WIFI_RETRY_DELAY).await;
        }

        if !matches!(controller.is_started(), Ok(true)) {
            let station_config = Config::Station(
                StationConfig::default()
                    .with_ssid(WIFI_SSID.try_into().unwrap())
                    .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                        WIFI_PASS.try_into().unwrap(),
                    )),
            );
            controller.set_config(&station_config).unwrap();
            controller.start_async().await.unwrap();
        }

        match controller.connect_async().await {
            Ok(_) => info!("[WIFI] Connected"),
            Err(e) => {
                info!("[WIFI] Connect failed: {:?}, retrying...", e);
                Timer::after(WIFI_RETRY_DELAY).await;
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
async fn run_tls_link(
    runner: zenoh_nano::link::tls::TlsLinkRunner<'static, &'static mut TcpSocket<'static>>,
) {
    if let Err(e) = runner.run().await {
        log::error!("[FAIL] tls link runner error: {:?}", e);
    }
}

#[embassy_executor::task]
async fn query_node_status(session: Session<'static>) {
    loop {
        let reply = match Get::new(session, NODE_STATUS_QUERY).await {
            Ok(v) => v,
            Err(e) => {
                log::error!("[FAIL] query failed: {:?}", e);
                return;
            }
        };

        match reply {
            GetResult::Ok(payload) => match core::str::from_utf8(payload.to_zslice().as_slice()) {
                Ok(text) => info!("[ZENOH] Node status: {}", text),
                Err(_) => info!(
                    "[ZENOH] Node status received (non-utf8, {} bytes)",
                    payload.to_zslice().as_slice().len()
                ),
            },
            GetResult::Err(err) => {
                info!(
                    "[ZENOH] Node replied with error ({} bytes)",
                    err.to_zslice().as_slice().len()
                );
            }
            GetResult::Timeout => info!("[ZENOH] Node status query timed out"),
            GetResult::NoReply => info!("[ZENOH] Node status query got no reply"),
        }
        Timer::after(QUERY_INTERVAL).await;
    }
}
