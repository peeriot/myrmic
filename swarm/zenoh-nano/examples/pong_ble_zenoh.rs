//! A very simple BLE-based ping-pong example for **Linux** where one peer periodically sends pings over a `test/ping` topic,
//! while the other receives the ping and replies with a ping on the `test/pong` topic
//!
//! NOTE:
//! Running the example is a bit **complicated**.
//! - First of all, you need two Linux machines each with a Bluetooth adapter, or one Linux machine with two Bluetooth adapters.
//!   One machine will run the `pong` peer (this example), the other the `ping` peer (the `z_ping` example from Zenoh).
//! - Second, running the `pong_ble_zenoh` example requires several preparatory steps, as outlined below.
//!
//! Run this `pong` peer example first using the following steps:
//! - The `pong_ble_zenoh` executable should have the `CAP_NET_ADMIN` capability, i.e. `sudo setcap cap_net_raw,cap_net_admin=eip ./pong_ble_zenoh`.
//! - The BT adapter should be down, i.e. `sudo hciconfig hci0 down`.
//! - On Ubuntu specifically, the executable MUST NOT be in the user's home directory, or else the caps won't work.
//! - If your adapter is not `hci0`, pass the numeric ID of the adapter (i.e. 0, 1, ...) to the executable as the one and only argument.
//!
//! Then, run the Zenoh `pong` peer first using:
//! TODO
//! ```sh
//! cd zenoh
//! cargo run --example z_ping bt_gatt/[adapter@]XX:XX:XX:XX:XX:XX
//! ```
//! ..where XX:XX:XX:XX:XX:XX is the MAC address of the Bluetooth adapter and/or the Linux machine running the `ping` peer.
#![deny(missing_docs)]
#![allow(clippy::uninlined_format_args)] // For `defmt`

use async_compat::Compat;

use bt_hci::controller::ExternalController;

use bt_hci_linux::Transport;

use embassy_executor::{Executor, Spawner};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_time::Duration;

use log::info;

use rand::RngCore;

use static_cell::StaticCell;

use trouble_host::prelude::*;

use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::trouble::{
    GattLink, GattLinkAcceptRunner, GattLinkReceive, GattLinkResources, GattLinkSend,
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

    let rng = RandomSource::new(mk_static!(LocalRng, LocalRng));

    let mut address = [0; 6];
    rng.fill_bytes(&mut address);

    let address = Address::random(address);
    info!("GATT address = {:?}", address);

    let stack = mk_static!(
        Stack<'static, PingController, DefaultPacketPool>,
        trouble_host::new(controller, stack_resources).build() //.set_random_address(address)
    );

    let stack_runner = stack.runner();

    spawner.spawn(ble_task(stack_runner).unwrap());

    info!("Initialization complete.");

    // Initialize link

    let link_resources = mk_static!(GattLinkResources, GattLinkResources::new());

    let link = mk_static!(GattLink<'static>, GattLink::new(link_resources));

    let (runner, receive, send) = link.accept(stack).await.unwrap();

    spawner.spawn(gatt_link_task(runner).unwrap());

    // Initialize network

    let pong_accept = Network::accept(
        receive,
        send,
        Duration::from_secs(30),
        rng,
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

/// Run the BLE task
///
/// Also listen to events so that we can discover Zenoh nodes advertising over BLE
#[embassy_executor::task]
async fn ble_task(mut runner: Runner<'static, PingController<'static>, DefaultPacketPool>) {
    runner.run().await.unwrap()
}

/// Run the GattLink connection
#[embassy_executor::task]
async fn gatt_link_task(runner: GattLinkAcceptRunner<'static, 'static, NoopRawMutex>) {
    runner.run().await.unwrap()
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
