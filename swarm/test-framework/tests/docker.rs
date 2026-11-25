//! Tests of [`test_framework::swarm`] on a docker network: hosts in containers, the swarm
//! attached through a router, and the network's faults. They need a docker daemon and built
//! `myrmic` and `zenoh-router` binaries (see [`test_framework::resolve_binary!`]).

use std::time::Duration;

use anyhow::Context as _;
use test_framework::{
    docker,
    host::{self, Remote},
    host_set, myrmic,
    overlay::{MulticastGroup, Overlay},
    start_runtimes,
    swarm::{self, Swarm},
    telemetry,
    wait::{DEFAULT_TIMEOUT, wait_for},
    world,
};

/// a CLI runtime in a container, not started
type DockerRt = swarm::StoppedRt<myrmic::StoppedRt<myrmic::Docker>>;

/// A swarm on a docker network of its own, attached through a router on a host of its own.
struct DockerSwarm {
    host_set: host_set::Docker,
    group: MulticastGroup,
    /// only kept alive: the swarm reaches its runtimes through it
    _router: host::Router<host::Docker>,
    swarm: Swarm,
}

impl DockerSwarm {
    async fn new() -> Self {
        let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
        let overlay = Overlay::new(&world![host_set]);
        let group = overlay.new_multicast_group();
        let router = host_set.new_host().await.router(&group).await;
        let swarm = Swarm::new(overlay.connect_to(&router)).await;
        Self {
            host_set,
            group,
            _router: router,
            swarm,
        }
    }

    /// a CLI runtime of the swarm on a new host of the network, not started, and its host
    async fn new_runtime(&self) -> (host::Docker, DockerRt) {
        let host = self.host_set.new_host().await;
        let rt = self.swarm.add(host.myrmic(&self.group).runtime());
        (host, rt)
    }
}

/// A swarm attached through a router reaches its runtimes on the network's hosts, and they reach
/// each other.
#[tokio::test(flavor = "multi_thread")]
async fn a_docker_swarm_connects_through_its_router() -> anyhow::Result<()> {
    let setup = DockerSwarm::new().await;
    let (_a_host, a) = setup.new_runtime().await;
    let (_b_host, b) = setup.new_runtime().await;
    start_runtimes!(setup.swarm; a, b);
    Ok(())
}

/// A runtime whose host is cut off drops out of the swarm: it no longer answers, and the others
/// lose their links to it.
#[tokio::test(flavor = "multi_thread")]
async fn an_isolated_hosts_runtime_drops_out_of_the_swarm() -> anyhow::Result<()> {
    let setup = DockerSwarm::new().await;
    let (_a_host, a) = setup.new_runtime().await;
    let (b_host, b) = setup.new_runtime().await;
    start_runtimes!(setup.swarm; a, b);

    b_host.isolate().await;
    setup.swarm.wait_until_connected().await?;
    Ok(())
}

/// Two blocked hosts' runtimes lose their link to each other, and both stay in the swarm.
#[tokio::test(flavor = "multi_thread")]
async fn blocked_hosts_lose_their_link() -> anyhow::Result<()> {
    let setup = DockerSwarm::new().await;
    let (a_host, a) = setup.new_runtime().await;
    let (b_host, b) = setup.new_runtime().await;
    start_runtimes!(setup.swarm; a, b);

    a_host.block(&b_host).await;
    setup.swarm.wait_until_connected().await?;
    Ok(())
}

/// Two hosts that were blocked from each other link again once they are unblocked.
#[tokio::test(flavor = "multi_thread")]
async fn unblocked_hosts_link_again() -> anyhow::Result<()> {
    let setup = DockerSwarm::new().await;
    let (a_host, a) = setup.new_runtime().await;
    let (b_host, b) = setup.new_runtime().await;
    start_runtimes!(setup.swarm; a, b);
    let block = a_host.block(&b_host).await;
    setup.swarm.wait_until_connected().await?;

    block.unblock().await;
    setup.swarm.wait_until_connected().await?;
    Ok(())
}

/// how long a degraded link delays each packet in these tests
const LINK_DELAY: Duration = Duration::from_millis(300);

/// a mebibyte in bytes
const MIB: u64 = 1024 * 1024;

/// Where the runtimes on a docker host keep their data: the CLI's data directory when it runs as
/// root.
const DATA_DIR: &str = "/root/.local/share/myrmic";

/// Every packet a host sends over a degraded link waits the link's delay.
#[tokio::test(flavor = "multi_thread")]
async fn a_degraded_link_delays_the_traffic_it_carries() -> anyhow::Result<()> {
    let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
    let (a, b) = (host_set.new_host().await, host_set.new_host().await);
    let b_address = get_address(&b).await?;

    a.degrade(&b, LINK_DELAY, Duration::ZERO, 0.0, None).await;
    let round_trip = get_round_trip(&a, &b_address).await?;
    anyhow::ensure!(
        round_trip >= LINK_DELAY,
        "a ping took {round_trip:?} over a link that delays by {LINK_DELAY:?}"
    );
    Ok(())
}

/// A degraded link delays only the traffic to its peer: the host's traffic to other hosts travels
/// as before.
#[tokio::test(flavor = "multi_thread")]
async fn a_degraded_link_leaves_the_hosts_other_links_alone() -> anyhow::Result<()> {
    let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
    let (a, b, c) = (
        host_set.new_host().await,
        host_set.new_host().await,
        host_set.new_host().await,
    );
    let c_address = get_address(&c).await?;

    a.degrade(&b, LINK_DELAY, Duration::ZERO, 0.0, None).await;
    let round_trip = get_round_trip(&a, &c_address).await?;
    anyhow::ensure!(
        round_trip < LINK_DELAY,
        "a ping to a third host took {round_trip:?} while the link to another one was degraded"
    );
    Ok(())
}

/// Restoring one of a host's degraded links leaves its other degraded links degraded.
#[tokio::test(flavor = "multi_thread")]
async fn restoring_one_link_leaves_another_degraded() -> anyhow::Result<()> {
    let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
    let (a, b, c) = (
        host_set.new_host().await,
        host_set.new_host().await,
        host_set.new_host().await,
    );
    let c_address = get_address(&c).await?;
    let to_b = a.degrade(&b, LINK_DELAY, Duration::ZERO, 0.0, None).await;
    a.degrade(&c, LINK_DELAY, Duration::ZERO, 0.0, None).await;

    to_b.restore().await;
    let round_trip = get_round_trip(&a, &c_address).await?;
    anyhow::ensure!(
        round_trip >= LINK_DELAY,
        "a ping over a link that delays by {LINK_DELAY:?} took {round_trip:?} once another link \
         was restored"
    );
    Ok(())
}

/// A link that loses every packet carries nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_that_loses_every_packet_carries_nothing() -> anyhow::Result<()> {
    let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
    let (a, b) = (host_set.new_host().await, host_set.new_host().await);
    let b_address = get_address(&b).await?;

    a.degrade(&b, Duration::ZERO, Duration::ZERO, 100.0, None)
        .await;
    let pinged = run_sh(&a, r#"ping -c 3 -i 0.2 -W 2 "$1""#, &[&b_address]).await?;
    anyhow::ensure!(
        !pinged.status.success(),
        "a ping got through a link that loses every packet: {pinged:?}"
    );
    Ok(())
}

/// A rate-limited link sends a packet no faster than its rate allows.
#[tokio::test(flavor = "multi_thread")]
async fn a_rate_limited_link_slows_large_packets() -> anyhow::Result<()> {
    /// the link's rate in bits per second
    const RATE_BPS: u64 = 1_000_000;
    /// a ping's payload in bytes, sent as fragments of one IP packet
    const PAYLOAD: u64 = 60_000;

    let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
    let (a, b) = (host_set.new_host().await, host_set.new_host().await);
    let b_address = get_address(&b).await?;

    a.degrade(&b, Duration::ZERO, Duration::ZERO, 0.0, Some(RATE_BPS))
        .await;
    let pinged = run_sh(
        &a,
        r#"ping -c 1 -s "$1" -W 10 "$2""#,
        &[&PAYLOAD.to_string(), &b_address],
    )
    .await?;
    anyhow::ensure!(pinged.status.success(), "ping failed: {pinged:?}");
    let round_trip = parse_round_trip(&String::from_utf8(pinged.stdout)?)?;
    // the payload's bits at the link's rate, without its headers
    let least = Duration::from_micros(PAYLOAD * 8 * 1_000_000 / RATE_BPS);
    anyhow::ensure!(
        round_trip >= least,
        "a ping of {PAYLOAD} bytes took {round_trip:?} over a link of {RATE_BPS} bit/s"
    );
    Ok(())
}

/// A restored link carries its traffic without the delay it had.
#[tokio::test(flavor = "multi_thread")]
async fn a_restored_link_carries_traffic_without_delay() -> anyhow::Result<()> {
    let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
    let (a, b) = (host_set.new_host().await, host_set.new_host().await);
    let b_address = get_address(&b).await?;
    let degraded = a.degrade(&b, LINK_DELAY, Duration::ZERO, 0.0, None).await;

    degraded.restore().await;
    let round_trip = get_round_trip(&a, &b_address).await?;
    anyhow::ensure!(
        round_trip < LINK_DELAY,
        "a ping took {round_trip:?} over a link restored from a delay of {LINK_DELAY:?}"
    );
    Ok(())
}

/// A traffic counter counts exactly the bytes a host sends to its peer and receives from it, not
/// its traffic with other hosts.
#[tokio::test(flavor = "multi_thread")]
async fn a_traffic_counter_counts_the_bytes_between_two_hosts() -> anyhow::Result<()> {
    /// how many pings `a` sends `b`
    const PINGS: u64 = 3;
    /// a ping's payload in bytes
    const PAYLOAD: u64 = 1000;
    /// a ping's IP packet in bytes: its payload, an ICMP header of 8 bytes and an IP header of
    /// 20 bytes; the answer is as large
    const PACKET: u64 = PAYLOAD + 8 + 20;

    let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
    let (a, b, c) = (
        host_set.new_host().await,
        host_set.new_host().await,
        host_set.new_host().await,
    );
    let (b_address, c_address) = (get_address(&b).await?, get_address(&c).await?);
    let counter = a.count_traffic(&b).await;

    for address in [&b_address, &c_address] {
        let pinged = run_sh(
            &a,
            r#"ping -c "$1" -i 0.2 -s "$2" -W 10 "$3""#,
            &[&PINGS.to_string(), &PAYLOAD.to_string(), address],
        )
        .await?;
        anyhow::ensure!(pinged.status.success(), "ping failed: {pinged:?}");
    }
    let (sent, received) = (
        counter.get_sent_bytes().await,
        counter.get_received_bytes().await,
    );
    anyhow::ensure!(
        sent == PINGS * PACKET && received == PINGS * PACKET,
        "{PINGS} pings of {PACKET} bytes counted as {sent} bytes sent and {received} received"
    );
    Ok(())
}

/// A process that holds more memory than its host's limit gets killed.
#[tokio::test(flavor = "multi_thread")]
async fn a_process_beyond_its_hosts_memory_limit_is_killed() -> anyhow::Result<()> {
    let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
    let host = host_set.new_host().await;

    host.set_memory_limit(64 * MIB).await;
    // the shell holds 200 MB in a variable
    let held = run_sh(
        &host,
        r#"held=$(head -c 200000000 /dev/zero | tr '\0' a); echo "${#held}""#,
        &[],
    )
    .await?;
    // 128 + 9: killed by SIGKILL, which the kernel sends when memory runs out
    anyhow::ensure!(
        held.status.code() == Some(137),
        "a process holding 200 MB on a host limited to 64 MiB ended with {held:?}"
    );
    Ok(())
}

/// A volume host's runtimes keep more data than its memory limit allows: the data doesn't count as
/// its memory.
#[tokio::test(flavor = "multi_thread")]
async fn a_volume_hosts_data_does_not_count_as_its_memory() -> anyhow::Result<()> {
    let host_set = host_set::Docker::new(&docker::Daemon::connect()).await;
    let host = host_set.new_volume_host().await;

    host.set_memory_limit(64 * MIB).await;
    let written = run_sh(
        &host,
        r#"head -c 200000000 /dev/zero > "$1/data" && sync"#,
        &[DATA_DIR],
    )
    .await?;
    anyhow::ensure!(
        written.status.success(),
        "writing 200 MB on a volume host limited to 64 MiB failed: {written:?}"
    );
    Ok(())
}

/// A runtime's peak memory is what its process has held at most.
#[tokio::test(flavor = "multi_thread")]
async fn a_runtimes_peak_memory_is_read_from_its_process() -> anyhow::Result<()> {
    let setup = DockerSwarm::new().await;
    let (_a_host, a) = setup.new_runtime().await;
    start_runtimes!(setup.swarm; a);

    let peak = a.get_peak_memory().await?;
    // A runtime holds some 70 MiB. Its virtual memory, which isn't what it holds, peaks at about
    // 1.5 GB.
    anyhow::ensure!(
        (10 * MIB..512 * MIB).contains(&peak),
        "a running runtime held at most {peak} bytes"
    );
    Ok(())
}

/// A runtime's counters are read from the telemetry files it exports: here, the replication
/// messages it receives for the `sys` namespace, which every runtime replicates.
#[tokio::test(flavor = "multi_thread")]
async fn a_runtimes_counters_are_read_from_its_telemetry_files() -> anyhow::Result<()> {
    /// where the runtimes export their telemetry
    const TELEMETRY_DIR: &str = "/tmp/myrmic-telemetry";

    let setup = DockerSwarm::new().await;
    let telemetry_config = swarm_telemetry::TelemetryConfig {
        file_export_dir: Some(TELEMETRY_DIR.into()),
        metrics: swarm_telemetry::config::metrics::Config {
            export_interval_ms: Some(500),
            ..Default::default()
        },
        ..Default::default()
    };
    let a_host = setup.host_set.new_host().await;
    let a = setup.swarm.add(
        a_host
            .myrmic(&setup.group)
            .runtime()
            .telemetry(telemetry_config.clone()),
    );
    let b_host = setup.host_set.new_host().await;
    let b = setup.swarm.add(
        b_host
            .myrmic(&setup.group)
            .runtime()
            .telemetry(telemetry_config),
    );
    start_runtimes!(setup.swarm; a, b);

    let files = telemetry::Files::new(vec![a_host], TELEMETRY_DIR.to_owned());
    wait_for(
        DEFAULT_TIMEOUT,
        "`a` to count replication messages for `sys`",
        || async {
            let received = files
                .get_counter("repl_msgs_recv", &[("subject", "sys/*/*")])
                .await;
            (received > 0).then_some(received)
        },
    )
    .await?;
    Ok(())
}

/// Run the shell script `script` on `host`, with `args` as its `$1`, `$2`, ..., and capture its
/// output.
async fn run_sh(
    host: &impl Remote,
    script: &str,
    args: &[&str],
) -> anyhow::Result<std::process::Output> {
    let mut sh_args = vec!["-c", script, "sh"];
    sh_args.extend(args);
    let output = tokio::process::Command::from(host.command("sh", &sh_args))
        .output()
        .await
        .with_context(|| format!("failed to run `{script}`"))?;
    Ok(output)
}

/// `host`'s address on its network, as the host itself reports it
async fn get_address(host: &impl Remote) -> anyhow::Result<String> {
    let listed = run_sh(
        host,
        "ip -o -4 addr show scope global | awk '{ split($4, address, \"/\"); print address[1]; exit }'",
        &[],
    )
    .await?;
    anyhow::ensure!(
        listed.status.success(),
        "listing the addresses failed: {listed:?}"
    );
    Ok(String::from_utf8(listed.stdout)?.trim().to_owned())
}

/// the round trip of one ping from `from` to the host at `address`
async fn get_round_trip(from: &impl Remote, address: &str) -> anyhow::Result<Duration> {
    let pinged = run_sh(from, r#"ping -c 1 -W 10 "$1""#, &[address]).await?;
    anyhow::ensure!(pinged.status.success(), "ping failed: {pinged:?}");
    parse_round_trip(&String::from_utf8(pinged.stdout)?)
}

/// the average round trip in `ping`'s output `stdout`
fn parse_round_trip(stdout: &str) -> anyhow::Result<Duration> {
    // `rtt min/avg/max/mdev = 300.212/300.212/300.212/0.000 ms`
    let millis: f64 = stdout
        .lines()
        .find_map(|line| line.strip_prefix("rtt min/avg/max/mdev = "))
        .and_then(|times| times.split('/').nth(1))
        .with_context(|| format!("ping reported no round trip: {stdout}"))?
        .parse()?;
    Ok(Duration::from_secs_f64(millis / 1000.0))
}
