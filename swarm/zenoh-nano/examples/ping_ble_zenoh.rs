//! A very simple BLE-based ping-pong example for **Linux** where one peer periodically sends pings over a `test/ping` topic,
//! while the other receives the ping and replies with a ping on the `test/pong` topic
//!
//! NOTE:
//! Running the example is a bit **complicated**.
//! - First of all, you need two Linux machines each with a Bluetooth adapter, or one Linux machine with two Bluetooth adapters.
//!   One machine will run the `ping` peer (this example), the other the `pong` peer (the `z_pong` example from Zenoh).
//! - Second, running the `ping_ble_zenoh` example requires several preparatory steps, as outlined below.
//!
//! Run the Zenoh `pong` peer first using:
//! TODO
//! ```sh
//! cd zenoh
//! cargo run --example z_pong bt_gatt/[adapter@]XX:XX:XX:XX:XX:XX
//! ```
//! ..where XX:XX:XX:XX:XX:XX is the MAC address of the Bluetooth adapter and/or the Linux machine running the `pong` peer.
//!
//! Then run this `ping` peer example using the following steps:
//! - The `ping_ble_zenoh` executable should have the `CAP_NET_ADMIN` capability, i.e. `sudo setcap cap_net_raw,cap_net_admin=eip ./ping_ble_zenoh`.
//! - The BT adapter should be down, i.e. `sudo hciconfig hci0 down`.
//! - On Ubuntu specifically, the executable MUST NOT be in the user's home directory, or else the caps won't work.
//! - If your adapter is not `hci0`, pass the numeric ID of the adapter (i.e. 0, 1, ...) to the executable as the one and only argument.
//!
//! Scouting:
//! The example does scan for BLE ads so as to detect BLE nodes advertising the Zenoh service and then connects to the first found.
#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

use async_compat::Compat;

use bt_hci::controller::ExternalController;
use bt_hci::event::Vendor;
use bt_hci_linux::Transport;

use embassy_executor::{Executor, Spawner};
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};

use log::info;

use rand::RngCore;

use static_cell::StaticCell;

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

extern crate alloc;

// Topics
const PING_TOPIC: &str = "test/ping";
const PONG_TOPIC: &str = "test/pong";

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

type PingHostResources =
    trouble_host::HostResources<PingController<'static>, DefaultPacketPool, 1, 2>;
type PingController<'d> = ExternalController<Transport, 20>;

/// Main task
#[embassy_executor::task]
async fn main_task(spawner: Spawner) {
    info!("Starting...");

    // Necessary to interoperate with `bt-hci-linux` which uses `tokio` internally
    Compat::new(main_task_tokio(spawner)).await;
}

async fn main_task_tokio(spawner: Spawner) {
    let dev = match std::env::args().collect::<Vec<_>>()[..] {
        [_] => 0,
        [_, ref s] => s.parse::<u16>().expect("Could not parse device number"),
        _ => panic!(
            "Provide the device number as the one and only command line argument, or no arguments to use device 0."
        ),
    };
    let transport = Transport::new(dev).unwrap();
    let controller = ExternalController::<_, 20>::new(transport);

    let stack_resources = mk_static!(PingHostResources, PingHostResources::new());

    let stack = mk_static!(
        Stack<'static, PingController, DefaultPacketPool>,
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

    let rng = RandomSource::new(mk_static!(LocalRng, LocalRng));

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
