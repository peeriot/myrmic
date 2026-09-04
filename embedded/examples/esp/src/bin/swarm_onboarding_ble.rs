//! Example of a device onboarding over Zenoh (with `zenoh-nano`) and BLE via `trouble-host`.
//!
//! The example models the "device" peer in the onboarding process.
//!
//! To run the example, first start `swarm` with:
//! ```sh
//! cargo run --features test-onboarding-plugin --bin swarm ./config/test_onboarding.jsonnet
//! ```
#![no_std]
#![no_main]
#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

use bt_hci::controller::ExternalController;
use bt_hci::event::Vendor;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Duration;

use esp_alloc::heap_allocator;
use esp_backtrace as _;
use esp_hal::ram;
use esp_hal::rng::Trng;
use esp_hal::timer::timg::TimerGroup;
use esp_metadata_generated::memory_range;
use esp_radio::ble::controller::BleConnector;

use log::info;

use trouble_host::prelude::*;

use swarm_onboarding::io::{SliceConsumer, SliceProducer};
use swarm_onboarding::qr::{Qr, QrPayload, QrTextType};
use swarm_onboarding::zenoh::device::DeviceOnboarding;
use swarm_onboarding::{DeviceKeys, DeviceProfile, OpNetFlags};

use x509_cert::Certificate;
use x509_cert::der::Decode;

use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::trouble::{
    GattLink, GattLinkConnectRunner, GattLinkReceive, GattLinkResources, GattLinkSend, scan,
    zenoh_addrs,
};
use zenoh_nano::network::Network;
use zenoh_nano::rng::RandomSource;
use zenoh_nano::scout::ZenohIdProto;
use zenoh_nano::session::{Session, SessionResources, SessionRunner};

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

type OnboardingHostResources =
    trouble_host::HostResources<OnboardingController<'static>, DefaultPacketPool, 1, 2>;
type OnboardingController<'d> = ExternalController<BleConnector<'d>, 20>;

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
        OnboardingController::new(BleConnector::new(peripherals.BT, Default::default()).unwrap());

    let stack_resources = mk_static!(OnboardingHostResources, OnboardingHostResources::new());

    let stack = mk_static!(
        Stack<'static, OnboardingController, DefaultPacketPool>,
        trouble_host::new(controller, stack_resources).build()
    );

    let stack_runner = stack.runner();

    let scout_signal = &*mk_static!(Signal<NoopRawMutex, Address>, Signal::new());

    spawner.spawn(ble_task(stack_runner, scout_signal).unwrap());

    info!("Initialization complete.");

    // Initialize link

    let link_resources = mk_static!(GattLinkResources, GattLinkResources::new());

    let link = mk_static!(GattLink<'static>, GattLink::new(link_resources));

    let addr = scout(stack, scout_signal).await;

    let (runner, receive, send) = link.connect(stack, addr).await.unwrap();

    spawner.spawn(gatt_link_task(runner).unwrap());

    // Initialize network

    let rng = mk_static!(esp_hal::rng::Rng, esp_hal::rng::Rng::new());

    let rng = RandomSource::new(rng);

    let network = Network::connect(
        receive,
        send,
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

/// Run the BLE task
///
/// Also listen to events so that we can discover Zenoh nodes advertising over BLE
#[embassy_executor::task]
async fn ble_task(
    mut runner: Runner<'static, OnboardingController<'static>, DefaultPacketPool>,
    scout_signal: &'static Signal<NoopRawMutex, Address>,
) {
    struct Handler<'a>(&'a Signal<NoopRawMutex, Address>);

    impl EventHandler for Handler<'_> {
        fn on_vendor(&self, _vendor: &Vendor) {}

        fn on_adv_reports(&self, reports: bt_hci::param::LeAdvReportsIter) {
            if let Some(addr) = zenoh_addrs(reports).next() {
                info!("Discovered Zenoh node at BLE address: {:?}", addr);

                self.0.signal(addr);
            }
        }

        fn on_ext_adv_reports(&self, reports: bt_hci::param::LeExtAdvReportsIter) {
            if let Some(addr) = zenoh_addrs(reports).next() {
                info!("Discovered Zenoh node at BLE address: {:?}", addr);

                self.0.signal(addr);
            }
        }
    }

    runner
        .run_with_handler(&Handler(scout_signal))
        .await
        .unwrap()
}

/// Run the GattLink connection
#[embassy_executor::task]
async fn gatt_link_task(
    runner: GattLinkConnectRunner<'static, 'static, OnboardingController<'static>, NoopRawMutex>,
) {
    runner.run().await.unwrap()
}

/// Scouting function: discovers a Zenoh node using BLE advertisements
async fn scout<'s>(
    stack: &'s Stack<'s, OnboardingController<'static>, DefaultPacketPool>,
    scout_signal: &Signal<NoopRawMutex, Address>,
) -> Address {
    info!("Scouting for Zenoh nodes...");

    match select(scan(stack), scout_signal.wait()).await {
        Either::First(res) => panic!("Unexpected end of scan: {:?}", res),
        Either::Second(addr) => {
            info!("Found Zenoh node at BLE address: {:?}", addr);
            addr
        }
    }
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
    network: Network<'static, GattLinkReceive<'static>, GattLinkSend<'static>>,
) {
    runner.run(network).await.unwrap()
}
