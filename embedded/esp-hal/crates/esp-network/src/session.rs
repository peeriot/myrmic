//! WiFi + embassy-net bring-up and the zenoh session lifecycle.

use alloc::vec;
use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_nal::io::Error;
use edge_nal::{MulticastV4, TcpConnect, TcpSplit, UdpBind, UdpReceive, UdpSend, UdpSplit};
use edge_nal_embassy::{Tcp, TcpBuffers, Udp, UdpBuffers};
use embassy_futures::select::{Either, select};
use embassy_net::{Runner, Stack, StackResources};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ConnectionError, ControllerConfig, DisconnectReason,
    Interface, WifiController,
};
use static_cell::{ConstStaticCell, StaticCell};
use zenoh_nano::buffers::ZSlice;
use zenoh_nano::dispatch::SubscriberPool;
use zenoh_nano::link::{LinkError, StreamingLinkReceive, StreamingLinkSend};
use zenoh_nano::network::Network;
use zenoh_nano::rng::RandomSource;
use zenoh_nano::scout::{
    SCOUT_BROADCAST_IP_ADDR, SCOUT_BROADCAST_PORT, SCOUT_MTU, ScoutLinkReceive, ScoutLinkSend,
    WhatAmIMatcher,
};
use zenoh_nano::session::{Session, SessionResources};

use crate::clock;

/// Backoff delay between Zenoh reconnect attempts
const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);
/// Backoff delay between WiFi reconnect attempts
const WIFI_RECONNECT_BACKOFF: Duration = Duration::from_secs(5);
/// Zenoh session lease duration
pub const SESSION_LEASE: Duration = Duration::from_secs(30);
/// Buffer size (bytes) for the streaming link framer
const LINK_BUFFER_SIZE: u16 = 100;
/// Key-expression prefix for our liveliness token.
///
/// Must stay in sync with `intro_liveliness` in `introspection-common`
/// (`@introspection/@liveliness/${zid:*}`), which the swarm's introspection plugin monitors to
/// detect nodes joining and leaving.
const LIVELINESS_KE_PREFIX: &str = "@introspection/@liveliness/";

/// This device's stable zenoh id, derived from the factory-burned base MAC:
/// the same board keeps the same identity across reboots, so exec
/// registration, cell placements and the liveliness token all survive one.
fn stable_zid() -> zenoh_nano::scout::ZenohIdProto {
    let mac = esp_hal::efuse::base_mac_address();
    let mut bytes = [0u8; 8];
    bytes[..6].copy_from_slice(mac.as_bytes());
    // Fixed nonzero tail: zenoh ids must not end in zero bytes.
    bytes[6] = 0x6d; // 'm'
    bytes[7] = 0x79; // 'y'
    #[expect(clippy::expect_used, reason = "tail bytes are statically nonzero")]
    zenoh_nano::scout::ZenohIdProto::try_from(&bytes[..]).expect("id bytes end nonzero")
}

/// Set your Wifi SSID via the `WIFI_SSID` environment variable
const WIFI_SSID: &str = if let Some(wifi_ssid) = option_env!("WIFI_SSID") {
    wifi_ssid
} else {
    "test"
};

/// Set your Wifi password via the `WIFI_PASS` environment variable
const WIFI_PASS: &str = if let Some(wifi_pass) = option_env!("WIFI_PASS") {
    wifi_pass
} else {
    "test"
};

/// Set a direct TCP address to connect to the zenoh network (bypasses scouting)
const TCP_DIRECT_ADDR: Option<&str> = option_env!("TCP_DIRECT_ADDR");

/// Builds the WiFi controller and the embassy-net stack. Called exactly once;
/// the caller spawns [`connection`], the stack runner and [`zenoh_session`].
///
/// # Panics
///
/// Panics if the WiFi module cannot be initialized, or when called twice.
#[must_use]
pub fn init_stack(
    wifi: WIFI<'static>,
) -> (
    WifiController<'static>,
    Stack<'static>,
    Runner<'static, Interface>,
) {
    static STACK_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let stack_resources = STACK_RESOURCES.init(StackResources::new());

    let station_config = station_config();
    #[expect(
        clippy::expect_used,
        reason = "If WiFi is broken, this is unrecoverable"
    )]
    let controller = esp_radio::wifi::WifiController::new(
        wifi,
        ControllerConfig::default().with_initial_config(station_config),
    )
    .expect("WiFi module cannot be initialized");
    let wifi_interface = esp_radio::wifi::Interface::station();

    let config = embassy_net::Config::dhcpv4(Default::default());

    let rng = Rng::new();
    let seed = u64::from(rng.random()) << 32 | u64::from(rng.random());

    // Init network stack
    let (stack, runner) = embassy_net::new(wifi_interface, config, stack_resources, seed);

    (controller, stack, runner)
}

/// The Zenoh session keeper: owns the session lifecycle and reconnects on
/// peer disconnect. Once the session exists, `on_session` builds the
/// session-scoped services future (the caller composes what runs on the
/// session), which is then driven alongside the supervision loop; `liveness`
/// is invoked once per liveness round.
pub async fn zenoh_session<F, Fut>(
    stack: embassy_net::Stack<'static>,
    on_session: F,
    liveness: fn(),
) where
    F: FnOnce(Session<'static, NoopRawMutex>) -> Fut,
    Fut: core::future::Future<Output = ()>,
{
    // All cell takes/inits must stay outside the reconnect loop - they panic on a
    // second call, so these must only run once for the lifetime of the task.
    static UDP_BUFFERS: ConstStaticCell<UdpBuffers<1>> = ConstStaticCell::new(UdpBuffers::new());
    static TCP_BUFFERS: ConstStaticCell<TcpBuffers<1>> = ConstStaticCell::new(TcpBuffers::new());
    static TCP_STACK: StaticCell<Tcp<'_>> = StaticCell::new();
    // This is Sync (wraps UnsafeCell), letting us hold a &'static SessionResources
    // even though SessionResources<NoopRawMutex> is !Sync. Session::new now takes &'a
    // (not &'a mut), so this shared reference suffices and yields Session<'static> as
    // required by the Zenoh request handlers.
    static SESSION_CELL: ConstStaticCell<SessionResources> =
        ConstStaticCell::new(SessionResources::new());
    // The dispatcher's consumer-slot pool. Shares the same lifetime/Sync story as SESSION_CELL.
    static SUBSCRIBER_POOL: ConstStaticCell<SubscriberPool> =
        ConstStaticCell::new(SubscriberPool::new());
    // The device's hybrid logical clock. Outlives individual transport sessions so
    // learned swarm time survives reconnects.
    static CLOCK: StaticCell<clock::SwarmClock> = StaticCell::new();
    // Session-scoped signals and tasks can survive transport reconnects because they
    // operate over the shared SessionResources above.
    let udp_buffers = UDP_BUFFERS.take();
    let tcp_buffers = TCP_BUFFERS.take();
    let tcp_stack = TCP_STACK.init(Tcp::new(stack, tcp_buffers));
    let session_res: &'static SessionResources = SESSION_CELL.take();
    let subscriber_pool: &'static SubscriberPool = SUBSCRIBER_POOL.take();

    // Initialize link
    stack.wait_link_up().await;
    log::info!("Waiting to get IP address...");
    stack.wait_config_up().await;

    let mut rng = Rng::new();
    let rng_source = RandomSource::new(&mut rng);

    let (session, mut session_runner) = Session::new(session_res, subscriber_pool);
    session.enable_liveliness(LIVELINESS_KE_PREFIX);
    // Must precede the client spawns below: publishers capture the clock when declared.
    session.set_clock(CLOCK.init(clock::SwarmClock::new(stable_zid())));
    wasm_runtime::init_wall_clock(clock::wall_time);

    let services = on_session(session);

    let supervise = async {
        let mut link_dropped = false;

        loop {
            // Liveness (observed): blocks on link state for unbounded time.
            liveness();

            if link_dropped {
                stack.wait_config_down().await;
                link_dropped = false;
            }
            stack.wait_config_up().await;

            let our_ip = match stack.config_v4() {
                Some(cfg) => cfg.address.address(),
                None => {
                    log::warn!(
                        "IP config disappeared after wait_config_up, retrying in {RECONNECT_BACKOFF:?}"
                    );
                    Timer::after(RECONNECT_BACKOFF).await;
                    continue;
                }
            };
            log::info!("Got IP: {our_ip}");

            let Some(peer_addr) = get_peer_addr(stack, udp_buffers, our_ip).await else {
                // None means we have to bring the link up again
                link_dropped = true;
                Timer::after(RECONNECT_BACKOFF).await;
                continue;
            };

            let mut tcp_socket = match tcp_stack.connect(peer_addr).await {
                Ok(s) => {
                    // Make known to the rest of the system that we are connected
                    CONNECTED.signal(());
                    s
                }
                Err(e) => {
                    log::warn!("TCP connect failed: {e:?}, retrying in {RECONNECT_BACKOFF:?}");
                    Timer::after(RECONNECT_BACKOFF).await;
                    continue;
                }
            };
            let (zenoh_read, zenoh_write) = tcp_socket.split();

            let zenoh_network = match Network::connect(
                StreamingLinkReceive::new(zenoh_read, LINK_BUFFER_SIZE),
                StreamingLinkSend::new(zenoh_write, LINK_BUFFER_SIZE),
                SESSION_LEASE,
                rng_source.clone(),
                stable_zid(),
            )
            .await
            {
                Ok(n) => n,
                Err(e) => {
                    log::warn!(
                        "Zenoh network connect failed: {e:?}, retrying in {RECONNECT_BACKOFF:?}"
                    );
                    Timer::after(RECONNECT_BACKOFF).await;
                    continue;
                }
            };

            if let Err(e) = session_runner.run(zenoh_network).await {
                log::warn!("Zenoh session encountered an error: {e}");
            }

            // Do not set link_dropped here: the WiFi may still be up (e.g. only the
            // swarm controller restarted). wait_config_up() at the top of the loop
            // will block if the IP is gone; if it's still valid we scout immediately.
            log::warn!("Zenoh session ended, reconnecting in {RECONNECT_BACKOFF:?}...");
            Timer::after(RECONNECT_BACKOFF).await;
        }
    };

    embassy_futures::join::join(services, supervise).await;
}

/// Establishes and keeps a WiFi connection. `liveness` is invoked once per
/// liveness round.
pub async fn connection(mut controller: WifiController<'static>, liveness: fn()) {
    log::info!("start connection task");

    log::info!("Scan");
    let scan_config = ScanConfig::default().with_max(10);
    match controller.scan_async(&scan_config).await {
        Ok(result) => {
            for ap in result {
                log::info!("{:?}", ap);
            }
        }
        Err(e) => log::warn!("Wifi scan failed: {e:?}"),
    }

    loop {
        // Liveness (observed): blocks on radio events for unbounded time.
        liveness();

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
                if let ConnectionError::Failed(info) = e
                    && matches!(info.reason, DisconnectReason::AuthenticationExpired)
                {
                    // This is a soft error. It is actually expected to happen on reboot
                    log::warn!("Failed to connect to wifi: {e:?}");
                } else {
                    log::error!("Failed to connect to wifi: {e:?}");
                }
                reset_station(&mut controller);
                Timer::after(WIFI_RECONNECT_BACKOFF).await;
            }
        }
    }
}

/// Signals once the device is connected to the Myrmic network
pub static CONNECTED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Scouting function: discovers a Zenoh node using UDP scouting messages
async fn scout<S>(stack: S, our_ip: Ipv4Addr) -> SocketAddr
where
    S: UdpBind,
{
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

    log::info!("Scouting for Zenoh nodes...");
    #[expect(
        clippy::expect_used,
        reason = "If we can't bind ports, we can't join a network"
    )]
    let mut udp_socket = stack
        .bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            SCOUT_BROADCAST_PORT,
        ))
        .await
        .expect("Unable to bind UDP socket");

    #[expect(
        clippy::expect_used,
        reason = "If we can't join a v4, we can't join a network"
    )]
    udp_socket
        .join_v4(SCOUT_BROADCAST_IP_ADDR, our_ip)
        .await
        .expect("Unable to join v4 UDP socket");

    let (receive, send) = udp_socket.split();

    let mut socket_addr = None;

    // Run the scouting responder
    #[expect(
        clippy::expect_used,
        reason = "If we can't scout, we can't join any network, so we have to panic"
    )]
    zenoh_nano::scout::run(
        UdpScoutLink(receive),
        UdpScoutLink(send),
        WhatAmIMatcher::empty().peer().client().router(),
        None,
        |_, hello| {
            for locator in &hello.locators {
                if locator.protocol().as_ref() != "tcp" {
                    continue;
                }
                log::info!("Discovered Zenoh node at {}", locator);

                let Ok(candidate) = locator.address().as_ref().parse::<SocketAddr>() else {
                    continue;
                };
                let IpAddr::V4(ip) = candidate.ip() else {
                    continue;
                };

                // A peer announces every address it listens on, including ones that only mean
                // anything on the host itself: `127.0.0.1` (its own loopback) and `0.0.0.0`
                // (the wildcard it bound). Dialing either from here reaches this device, not
                // the peer, and the session then sits dead until the lease expires. Keep
                // scanning the hello for an address that is actually routable over the WiFi
                // link instead of taking the first TCP locator on offer.
                if ip.is_loopback() || ip.is_unspecified() {
                    log::debug!("Skipping non-routable locator {locator}");
                    continue;
                }

                socket_addr = Some(candidate);
                log::info!("Using Zenoh node at {candidate}");
                return true;
            }

            false
        },
    )
    .await
    .expect("Failed to run scouting");

    #[expect(
        clippy::unwrap_used,
        reason = "The scouting logic already ensures that it returns only
        when a Some(socket_addr) is found. This is unreachable"
    )]
    socket_addr.unwrap()
}

/// Obtains the peer address either by scouting or by using the [`TCP_DIRECT_ADDR`] env
///
/// # Returns
///
/// * `Some(peer_addr)`
/// * `None`: if the WiFi connection was lost during scouting
async fn get_peer_addr(
    stack: Stack<'_>,
    udp_buffers: &mut UdpBuffers<1>,
    our_ip: Ipv4Addr,
) -> Option<SocketAddr> {
    // Avoid UDP scouting if we have a valid usable direct TCP option
    match TCP_DIRECT_ADDR.map(|addr_str| {
        log::info!("Using direct TCP address: {addr_str}");
        addr_str.parse::<SocketAddr>()
    }) {
        Some(Ok(addr)) => return Some(addr),
        Some(Err(e)) => {
            log::error!("Invalid TCP_DIRECT_ADDR format, must be IP:PORT {e}");
            log::warn!("Falling back to UDP scouting");
        }
        None => {}
    }

    // UDP scout
    match select(
        scout(Udp::new(stack, udp_buffers), our_ip),
        stack.wait_link_down(),
    )
    .await
    {
        Either::First(addr) => Some(addr),
        Either::Second(()) => {
            log::warn!("WiFi dropped during scouting, waiting for reconnect...");
            None
        }
    }
}

/// Build the station configuration from the compile-time WiFi credentials.
#[expect(
    clippy::expect_used,
    reason = "the credentials are compile-time constants; malformed ones are unrecoverable"
)]
fn station_config() -> Config {
    let config = StationConfig::default()
        .with_ssid(WIFI_SSID.try_into().expect("SSID exceeds the driver limit"))
        .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
            WIFI_PASS
                .try_into()
                .expect("password exceeds the driver limit"),
        ));

    Config::Station(config)
}

/// Clear the driver's half-associated station state after a failed connect.
///
/// Re-applying the station configuration is the only public API that resets the station state.
/// Without this, every second `connect_async` fails immediately with `ConnectionFailed` and an
/// all-zero BSSID instead of reporting the real reason the attempt failed.
fn reset_station(controller: &mut WifiController<'static>) {
    if let Err(e) = controller.set_config(&station_config()) {
        log::warn!("Failed to reset station state after failed connect: {e:?}");
    }
}
