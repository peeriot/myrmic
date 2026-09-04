//! A very simple BLE-based ping-pong example for the ESP32 where one peer periodically sends pings over a `test/ping` topic,
//! while the other receives the ping and replies with a ping on the `test/pong` topic
//!
//! Run the Zenoh `pong` peer first using:
//! TODO
//! ```sh
//! cd zenoh
//! cargo run --example z_pong bt_gatt/[adapter@]XX:XX:XX:XX:XX:XX
//! ```
//!
//! Scouting:
//! The example does scan for BLE ads so as to detect BLE nodes advertising the Zenoh service and then connects to the first found.
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
use embassy_time::{Duration, Timer};

use esp_alloc::heap_allocator;
use esp_backtrace as _;
use esp_hal::ram;
use esp_hal::timer::timg::TimerGroup;
use esp_metadata_generated::memory_range;
use esp_radio::ble::controller::BleConnector;

use log::info;

use trouble_host::prelude::*;

use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::trouble::{
    GattLink, GattLinkConnectRunner, GattLinkReceive, GattLinkResources, GattLinkSend, scan,
    zenoh_addrs,
};
use zenoh_nano::network::Network;
use zenoh_nano::ops::publish::Publisher;
use zenoh_nano::ops::subscribe::Subscriber;
use zenoh_nano::rng::RandomSource;
use zenoh_nano::scout::ZenohIdProto;
use zenoh_nano::session::{Session, SessionResources, SessionRunner};

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

// Topics
const PING_TOPIC: &str = "test/ping";
const PONG_TOPIC: &str = "test/pong";

type PingHostResources =
    trouble_host::HostResources<PingController<'static>, DefaultPacketPool, 1, 2>;
type PingController<'d> = ExternalController<BleConnector<'d>, 20>;

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
        PingController::new(BleConnector::new(peripherals.BT, Default::default()).unwrap());

    let stack_resources = mk_static!(PingHostResources, PingHostResources::new());

    let stack = mk_static!(
        Stack<'static, PingController<'_>, DefaultPacketPool>,
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

    let ping_network = Network::connect(
        receive,
        send,
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

/// Run the BLE task
///
/// Also listen to events so that we can discover Zenoh nodes advertising over BLE
#[embassy_executor::task]
async fn ble_task(
    mut runner: Runner<'static, PingController<'static>, DefaultPacketPool>,
    scout_signal: &'static Signal<NoopRawMutex, Address>,
) {
    struct Handler<'a>(&'a Signal<NoopRawMutex, Address>);

    impl EventHandler for Handler<'_> {
        fn on_vendor(&self, _vendor: &Vendor<'_>) {}

        fn on_adv_reports(&self, reports: bt_hci::param::LeAdvReportsIter<'_>) {
            if let Some(addr) = zenoh_addrs(reports).next() {
                info!("Discovered Zenoh node at BLE address: {:?}", addr);

                self.0.signal(addr);
            }
        }

        fn on_ext_adv_reports(&self, reports: bt_hci::param::LeExtAdvReportsIter<'_>) {
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
    runner: GattLinkConnectRunner<'static, 'static, PingController<'static>, NoopRawMutex>,
) {
    runner.run().await.unwrap()
}

/// Scouting function: discovers a Zenoh node using BLE advertisements
async fn scout<'s>(
    stack: &'s Stack<'s, PingController<'static>, DefaultPacketPool>,
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
    network: Network<'static, GattLinkReceive<'static>, GattLinkSend<'static>>,
) {
    runner.run(network).await.unwrap()
}
