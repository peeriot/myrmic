use allure_cargotest::{allure_test, step};
use test_framework::{
    compose::ComposeProject,
    docker::init_docker,
    sidecar::{Sidecar, ZenohMode},
    swarm::SwarmImage,
};
use tokio::time::Instant;

#[allure_test]
#[tokio::test(flavor = "multi_thread")]
async fn network_tests() {
    // prepare docker images
    let docker = init_docker();

    Sidecar::build(
        &docker,
        "assets/dockerfiles/sidecar.dockerfile",
        "../../target/release/test-sidecar",
        "sidecar:network_tests",
    )
    .await;

    let test_router_jsonnet = std::path::PathBuf::from("../../swarm/configs/test_router.jsonnet");
    let test_peer_jsonnet = std::path::PathBuf::from("../../swarm/configs/test_peer.jsonnet");
    let test_peer_gossip_jsonnet =
        std::path::PathBuf::from("../../swarm/configs/test_peer_gossip.jsonnet");
    let test_peer_no_scouting_jsonnet =
        std::path::PathBuf::from("../../swarm/configs/test_peer_no_scouting.jsonnet");
    SwarmImage::build(
        &docker,
        "assets/dockerfiles/swarm.dockerfile",
        "../../target/release/swarm",
        "swarm:network_tests",
        &[
            (test_router_jsonnet.as_path(), "test_router.jsonnet"),
            (test_peer_jsonnet.as_path(), "test_peer.jsonnet"),
            (
                test_peer_gossip_jsonnet.as_path(),
                "test_peer_gossip.jsonnet",
            ),
            (
                test_peer_no_scouting_jsonnet.as_path(),
                "test_peer_no_scouting.jsonnet",
            ),
        ],
    )
    .await;

    // run tests
    sorg_ctl_tool_in_routed_network().await;
    a1_discovery_multicast_peers().await;
    a1_discovery_no_multicast_peers().await;
    // no idea if and how this ever worked, codex came to the conclusion it fails for the same
    // reason the internal zenoh test is ignored:
    // https://github.com/peeriot/zenoh/blob/bdf4c76397e56fe35e01287fad4c00daeef196cf/zenoh/tests/routing.rs#L824C2-L826C53
    // a2_gossip_via_router().await;
    a3_partition_and_heal_peers().await;
    a4_dynamic_publisher_discovery().await;
    b1_single_level_wildcard().await;
    b2_multi_level_wildcard().await;
    c1_present_queryable().await;
    c2_absent_queryable().await;
    c3_partition_then_heal_queryable().await;
}

#[step]
async fn sorg_ctl_tool_in_routed_network() {
    // compose up
    let compose = ComposeProject::up(
        "assets/compose/sorg_ctl_tool_in_routed_network.yml",
        "sorg_ctl_tool_in_routed_network",
    )
    .await;

    // ask sidecar to list exec runtmes
    let instant = Instant::now();
    let sidecar = Sidecar::new("http://127.0.0.1:8080");
    let response = sidecar
        .retry_count_exec_runtimes_until(
            "tcp/router:7447",
            |response| {
                response.count == 10 || instant.elapsed() > std::time::Duration::from_secs(30)
            },
            tokio::time::Duration::from_secs(1),
        )
        .await;

    assert_eq!(Some(10), response.map(|r| r.count));

    // compose down
    compose.down().await;
}

#[step]
async fn a1_discovery_multicast_peers() {
    // compose up
    let compose = ComposeProject::up(
        "assets/compose/a1_discovery_multicast_peers.yml",
        "a1_discovery_multicast_peers",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17447");
    let peers = compose.service_containers("peer").await;
    assert_eq!(peers.len(), 2, "expected exactly 2 peer containers");
    let network_name = compose.network_name("network");

    let peer_1_zid = peers[0].zenoh_zid().await;
    let peer_2_zid = peers[1].zenoh_zid().await;

    let peer_1_endpoint = format!(
        "tcp/{}:{}",
        peers[0].container_ip(&network_name).await,
        peers[0].zenoh_tcp_port().await
    );
    let peer_2_endpoint = format!(
        "tcp/{}:{}",
        peers[1].container_ip(&network_name).await,
        peers[1].zenoh_tcp_port().await
    );

    let result = wait_for_peer_connectivity(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
        std::time::Duration::from_mins(1),
    )
    .await;

    if let Err(message) = result {
        panic!("{message}");
    }

    compose.down().await;
}

#[step]
async fn a1_discovery_no_multicast_peers() {
    let compose = ComposeProject::up(
        "assets/compose/a1_discovery_no_multicast_peers.yml",
        "a1_discovery_no_multicast_peers",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17449");
    let peers = compose.service_containers("peer").await;
    assert_eq!(peers.len(), 2, "expected exactly 2 peer containers");
    let network_name = compose.network_name("network");

    let peer_1_zid = peers[0].zenoh_zid().await;
    let peer_2_zid = peers[1].zenoh_zid().await;

    let peer_1_endpoint = format!(
        "tcp/{}:{}",
        peers[0].container_ip(&network_name).await,
        peers[0].zenoh_tcp_port().await
    );
    let peer_2_endpoint = format!(
        "tcp/{}:{}",
        peers[1].container_ip(&network_name).await,
        peers[1].zenoh_tcp_port().await
    );

    // Wait long enough for peers to have had the opportunity to discover each other — they
    // should remain disconnected because scouting is disabled.
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    let result = peers_connected(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
    )
    .await;

    match result {
        Ok(true) => panic!("peers unexpectedly discovered each other without scouting"),
        Ok(false) | Err(_) => {} // not connected — as expected
    }

    compose.down().await;
}

#[step]
async fn a2_gossip_via_router() {
    let compose = ComposeProject::up(
        "assets/compose/a2_gossip_via_router.yml",
        "a2_gossip_via_router",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17450");
    let peers_a = compose.service_containers("peer-a").await;
    let peers_b = compose.service_containers("peer-b").await;
    assert_eq!(peers_a.len(), 1, "expected exactly 1 peer-a container");
    assert_eq!(peers_b.len(), 1, "expected exactly 1 peer-b container");

    let peer_a_endpoint = format!(
        "tcp/{}:{}",
        peers_a[0]
            .container_ip(&compose.network_name("network_a"))
            .await,
        peers_a[0].zenoh_tcp_port().await
    );
    let peer_b_endpoint = format!(
        "tcp/{}:{}",
        peers_b[0]
            .container_ip(&compose.network_name("network_b"))
            .await,
        peers_b[0].zenoh_tcp_port().await
    );
    let peer_a_zid = peers_a[0].zenoh_zid().await;
    let peer_b_zid = peers_b[0].zenoh_zid().await;

    // Wait for gossip to propagate routing through the router
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    let subscriber = match sidecar
        .tc_create_subscriber(
            &peer_a_endpoint,
            ZenohMode::Client,
            &peer_a_zid,
            "tests/a2",
            Some(5),
            None,
        )
        .await
    {
        Ok(subscriber) => subscriber,
        Err(err) => {
            panic!("failed to create test-control subscriber on peer-a: {err}");
        }
    };
    if !subscriber.ok {
        panic!(
            "test-control subscriber creation on peer-a returned ok=false for {}",
            subscriber.key_expr
        );
    }

    // Allow subscription declaration to propagate through gossip
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let publisher = match sidecar
        .tc_create_publisher(
            &peer_b_endpoint,
            ZenohMode::Client,
            &peer_b_zid,
            "tests/a2",
            "hello",
            Some(5),
            Some(100),
        )
        .await
    {
        Ok(publisher) => publisher,
        Err(err) => {
            panic!("failed to create test-control publisher on peer-b: {err}");
        }
    };
    if !publisher.ok {
        panic!(
            "test-control publisher creation on peer-b returned ok=false for {}",
            publisher.key_expr
        );
    }

    // Allow messages to propagate
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let subscriber_stats = match sidecar
        .tc_stats(&peer_a_endpoint, ZenohMode::Client, &peer_a_zid, "tests/a2")
        .await
    {
        Ok(stats) => stats,
        Err(err) => {
            panic!("failed to read test-control subscriber stats: {err}");
        }
    };
    let publisher_stats = match sidecar
        .tc_stats(&peer_b_endpoint, ZenohMode::Client, &peer_b_zid, "tests/a2")
        .await
    {
        Ok(stats) => stats,
        Err(err) => {
            panic!("failed to read test-control publisher stats: {err}");
        }
    };

    assert_eq!(
        publisher_stats.sent, 5,
        "expected peer-b to send 5 messages via router, got {}",
        publisher_stats.sent
    );
    assert_eq!(
        subscriber_stats.received, 5,
        "expected peer-a to receive 5 messages via router, got {}",
        subscriber_stats.received
    );

    compose.down().await;
}

#[step]
async fn a3_partition_and_heal_peers() {
    let compose = ComposeProject::up(
        "assets/compose/a3_partition_and_heal_peers.yml",
        "a3_partition_and_heal_peers",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17451");
    let peers = compose.service_containers("peer").await;
    assert_eq!(peers.len(), 2, "expected exactly 2 peer containers");
    let network_name = compose.network_name("network");

    let peer_1_zid = peers[0].zenoh_zid().await;
    let peer_2_zid = peers[1].zenoh_zid().await;

    let peer_1_endpoint = format!(
        "tcp/{}:{}",
        peers[0].container_ip(&network_name).await,
        peers[0].zenoh_tcp_port().await
    );
    let peer_2_endpoint = format!(
        "tcp/{}:{}",
        peers[1].container_ip(&network_name).await,
        peers[1].zenoh_tcp_port().await
    );

    // Wait for initial peer connectivity
    let result = wait_for_peer_connectivity(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
        std::time::Duration::from_mins(1),
    )
    .await;
    if let Err(message) = result {
        panic!("peers did not connect initially: {message}");
    }

    // Partition: block traffic between the two peers
    let peer_1_ip = peers[0].container_ip(&network_name).await;
    let peer_2_ip = peers[1].container_ip(&network_name).await;
    peers[0].reject_remote(&network_name, &peer_2_ip).await;
    peers[1].reject_remote(&network_name, &peer_1_ip).await;

    // Wait for peers to disconnect
    let result = wait_for_peer_disconnection(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
        std::time::Duration::from_mins(1),
    )
    .await;
    if let Err(message) = result {
        panic!("peers did not disconnect after partition: {message}");
    }

    // Heal: remove the iptables rules
    peers[0].allow_all_traffic(&network_name).await;
    peers[1].allow_all_traffic(&network_name).await;

    // Wait for peers to reconnect
    let result = wait_for_peer_connectivity(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
        std::time::Duration::from_mins(1),
    )
    .await;

    if let Err(message) = result {
        panic!("peers did not reconnect after healing: {message}");
    }

    compose.down().await;
}

#[step]
async fn a4_dynamic_publisher_discovery() {
    let compose = ComposeProject::up(
        "assets/compose/a4_dynamic_publisher_discovery.yml",
        "a4_dynamic_publisher_discovery",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17452");
    let peers = compose.service_containers("peer").await;
    assert_eq!(peers.len(), 3, "expected exactly 3 peer containers");
    let network_name = compose.network_name("network");

    let peer_1_endpoint = format!(
        "tcp/{}:{}",
        peers[0].container_ip(&network_name).await,
        peers[0].zenoh_tcp_port().await
    );
    let peer_2_endpoint = format!(
        "tcp/{}:{}",
        peers[1].container_ip(&network_name).await,
        peers[1].zenoh_tcp_port().await
    );
    let peer_3_endpoint = format!(
        "tcp/{}:{}",
        peers[2].container_ip(&network_name).await,
        peers[2].zenoh_tcp_port().await
    );

    // Wait for all peers to discover each other
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    let sub_id = match sidecar
        .start_subscriber(&peer_1_endpoint, ZenohMode::Client, "tests/a4")
        .await
    {
        Ok(id) => id,
        Err(err) => {
            panic!("failed to start subscriber on peer-1: {err}");
        }
    };

    // Allow subscription to propagate
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // First publisher: peer-2 publishes 5 messages
    if let Err(err) = sidecar
        .publish(&peer_2_endpoint, ZenohMode::Client, "tests/a4", 5)
        .await
    {
        panic!("failed to publish from peer-2: {err}");
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let count_after_peer2 = match sidecar.subscriber_count(sub_id).await {
        Ok(c) => c,
        Err(err) => {
            panic!("failed to read subscriber count after peer-2: {err}");
        }
    };

    // Second publisher: peer-3 publishes 5 more messages
    if let Err(err) = sidecar
        .publish(&peer_3_endpoint, ZenohMode::Client, "tests/a4", 5)
        .await
    {
        panic!("failed to publish from peer-3: {err}");
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let count_after_peer3 = match sidecar.subscriber_count(sub_id).await {
        Ok(c) => c,
        Err(err) => {
            panic!("failed to read subscriber count after peer-3: {err}");
        }
    };

    assert_eq!(
        count_after_peer2, 5,
        "expected 5 messages from peer-2, got {count_after_peer2}"
    );
    assert_eq!(
        count_after_peer3, 10,
        "expected 10 total messages after peer-3, got {count_after_peer3}"
    );

    compose.down().await;
}

#[step]
async fn b1_single_level_wildcard() {
    let compose = ComposeProject::up(
        "assets/compose/b1_single_level_wildcard.yml",
        "b1_single_level_wildcard",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17453");
    let peers = compose.service_containers("peer").await;
    assert_eq!(peers.len(), 2, "expected exactly 2 peer containers");
    let network_name = compose.network_name("network");

    let peer_1_endpoint = format!(
        "tcp/{}:{}",
        peers[0].container_ip(&network_name).await,
        peers[0].zenoh_tcp_port().await
    );
    let peer_2_endpoint = format!(
        "tcp/{}:{}",
        peers[1].container_ip(&network_name).await,
        peers[1].zenoh_tcp_port().await
    );

    // Wait for peers to discover each other
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    // Subscribe with single-level wildcard on peer-1
    let sub_id = match sidecar
        .start_subscriber(&peer_1_endpoint, ZenohMode::Client, "foo/*/bar")
        .await
    {
        Ok(id) => id,
        Err(err) => {
            panic!("failed to start subscriber: {err}");
        }
    };

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Publish to matching key (single level)
    if let Err(err) = sidecar
        .publish(&peer_2_endpoint, ZenohMode::Client, "foo/x/bar", 1)
        .await
    {
        panic!("failed to publish to foo/x/bar: {err}");
    }

    // Publish to non-matching key (two levels — should not match foo/*/bar)
    if let Err(err) = sidecar
        .publish(&peer_2_endpoint, ZenohMode::Client, "foo/x/y/bar", 1)
        .await
    {
        panic!("failed to publish to foo/x/y/bar: {err}");
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let count = match sidecar.subscriber_count(sub_id).await {
        Ok(c) => c,
        Err(err) => {
            panic!("failed to read subscriber count: {err}");
        }
    };

    assert_eq!(
        count, 1,
        "expected exactly 1 match for foo/*/bar, got {count}"
    );

    compose.down().await;
}

#[step]
async fn b2_multi_level_wildcard() {
    let compose = ComposeProject::up(
        "assets/compose/b2_multi_level_wildcard.yml",
        "b2_multi_level_wildcard",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17454");
    let peers = compose.service_containers("peer").await;
    assert_eq!(peers.len(), 2, "expected exactly 2 peer containers");
    let network_name = compose.network_name("network");

    let peer_1_endpoint = format!(
        "tcp/{}:{}",
        peers[0].container_ip(&network_name).await,
        peers[0].zenoh_tcp_port().await
    );
    let peer_2_endpoint = format!(
        "tcp/{}:{}",
        peers[1].container_ip(&network_name).await,
        peers[1].zenoh_tcp_port().await
    );

    // Wait for peers to discover each other
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    // Subscribe with multi-level wildcard on peer-1
    let sub_id = match sidecar
        .start_subscriber(&peer_1_endpoint, ZenohMode::Client, "foo/**")
        .await
    {
        Ok(id) => id,
        Err(err) => {
            panic!("failed to start subscriber: {err}");
        }
    };

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Publish to foo/a — should match foo/**
    if let Err(err) = sidecar
        .publish(&peer_2_endpoint, ZenohMode::Client, "foo/a", 1)
        .await
    {
        panic!("failed to publish to foo/a: {err}");
    }

    // Publish to foo/a/b — should match foo/**
    if let Err(err) = sidecar
        .publish(&peer_2_endpoint, ZenohMode::Client, "foo/a/b", 1)
        .await
    {
        panic!("failed to publish to foo/a/b: {err}");
    }

    // Publish to bar/foo — should NOT match foo/**
    if let Err(err) = sidecar
        .publish(&peer_2_endpoint, ZenohMode::Client, "bar/foo", 1)
        .await
    {
        panic!("failed to publish to bar/foo: {err}");
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let count = match sidecar.subscriber_count(sub_id).await {
        Ok(c) => c,
        Err(err) => {
            panic!("failed to read subscriber count: {err}");
        }
    };

    assert_eq!(
        count, 2,
        "expected exactly 2 matches for foo/**, got {count}"
    );

    compose.down().await;
}

#[step]
async fn c1_present_queryable() {
    let compose = ComposeProject::up(
        "assets/compose/c1_present_queryable.yml",
        "c1_present_queryable",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17455");
    let peers = compose.service_containers("peer").await;
    assert_eq!(peers.len(), 2, "expected exactly 2 peer containers");
    let network_name = compose.network_name("network");

    let peer_1_endpoint = format!(
        "tcp/{}:{}",
        peers[0].container_ip(&network_name).await,
        peers[0].zenoh_tcp_port().await
    );
    let peer_2_endpoint = format!(
        "tcp/{}:{}",
        peers[1].container_ip(&network_name).await,
        peers[1].zenoh_tcp_port().await
    );

    // Wait for peers to discover each other
    let peer_1_zid = peers[0].zenoh_zid().await;
    let peer_2_zid = peers[1].zenoh_zid().await;
    let result = wait_for_peer_connectivity(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
        std::time::Duration::from_mins(1),
    )
    .await;
    if let Err(message) = result {
        panic!("peers did not connect: {message}");
    }

    // Register queryable on peer-2
    if let Err(err) = sidecar
        .start_queryable(&peer_2_endpoint, ZenohMode::Client, "tests/c1", "hello")
        .await
    {
        panic!("failed to start queryable on peer-2: {err}");
    }

    // Issue get from peer-1 — should reach peer-2's queryable
    let result = retry_zenoh_get(
        &sidecar,
        &peer_1_endpoint,
        "tests/c1",
        2000,
        1,
        std::time::Duration::from_secs(30),
    )
    .await;

    if let Err(message) = result {
        panic!("get did not receive reply from peer-2 queryable: {message}");
    }

    compose.down().await;
}

#[step]
async fn c2_absent_queryable() {
    let compose = ComposeProject::up(
        "assets/compose/c2_absent_queryable.yml",
        "c2_absent_queryable",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17456");
    let peers = compose.service_containers("peer").await;
    assert_eq!(peers.len(), 2, "expected exactly 2 peer containers");
    let network_name = compose.network_name("network");

    let peer_1_endpoint = format!(
        "tcp/{}:{}",
        peers[0].container_ip(&network_name).await,
        peers[0].zenoh_tcp_port().await
    );
    let peer_2_endpoint = format!(
        "tcp/{}:{}",
        peers[1].container_ip(&network_name).await,
        peers[1].zenoh_tcp_port().await
    );

    // Wait for peers to discover each other
    let peer_1_zid = peers[0].zenoh_zid().await;
    let peer_2_zid = peers[1].zenoh_zid().await;
    let result = wait_for_peer_connectivity(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
        std::time::Duration::from_mins(1),
    )
    .await;
    if let Err(message) = result {
        panic!("peers did not connect: {message}");
    }

    // Issue a get with no queryable registered — expect 0 replies
    let replies = sidecar
        .zenoh_get(&peer_1_endpoint, ZenohMode::Client, "tests/c2", 2000)
        .await;

    match replies {
        Ok(count) => assert_eq!(count, 0, "expected 0 replies (no queryable), got {count}"),
        Err(err) => panic!("zenoh get returned error: {err}"),
    }

    compose.down().await;
}

#[step]
async fn c3_partition_then_heal_queryable() {
    let compose = ComposeProject::up(
        "assets/compose/c3_partition_then_heal_queryable.yml",
        "c3_partition_then_heal_queryable",
    )
    .await;

    let sidecar = Sidecar::new("http://127.0.0.1:17448");
    let peers = compose.service_containers("peer").await;
    assert_eq!(peers.len(), 2, "expected exactly 2 peer containers");
    let network_name = compose.network_name("network");

    let peer_1_zid = peers[0].zenoh_zid().await;
    let peer_2_zid = peers[1].zenoh_zid().await;

    let peer_1_endpoint = format!(
        "tcp/{}:{}",
        peers[0].container_ip(&network_name).await,
        peers[0].zenoh_tcp_port().await
    );
    let peer_2_endpoint = format!(
        "tcp/{}:{}",
        peers[1].container_ip(&network_name).await,
        peers[1].zenoh_tcp_port().await
    );

    // Wait for initial peer connectivity
    let result = wait_for_peer_connectivity(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
        std::time::Duration::from_mins(1),
    )
    .await;
    if let Err(message) = result {
        panic!("{message}");
    }

    // Register a queryable on peer-2 via the sidecar
    if let Err(err) = sidecar
        .start_queryable(&peer_2_endpoint, ZenohMode::Client, "tests/c3", "hello")
        .await
    {
        panic!("failed to start queryable on peer-2: {err}");
    }

    // Issue a get from peer-1 — should reach peer-2's queryable
    let result = retry_zenoh_get(
        &sidecar,
        &peer_1_endpoint,
        "tests/c3",
        1000,
        1,
        std::time::Duration::from_secs(30),
    )
    .await;
    if let Err(message) = result {
        panic!("get before partition failed: {message}");
    }

    // Partition: block traffic between the two peers using iptables
    let peer_1_ip = peers[0].container_ip(&network_name).await;
    let peer_2_ip = peers[1].container_ip(&network_name).await;
    peers[0].reject_remote(&network_name, &peer_2_ip).await;
    peers[1].reject_remote(&network_name, &peer_1_ip).await;

    // Wait for peers to disconnect from each other
    let result = wait_for_peer_disconnection(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
        std::time::Duration::from_mins(1),
    )
    .await;
    if let Err(message) = result {
        panic!("peers did not disconnect after partition: {message}");
    }

    // Get during partition — should time out (0 replies)
    let replies = sidecar
        .zenoh_get(&peer_1_endpoint, ZenohMode::Client, "tests/c3", 1000)
        .await;
    match replies {
        Ok(count) if count > 0 => {
            panic!("expected 0 replies during partition, got {count}");
        }
        Ok(_) => {} // 0 replies as expected
        Err(err) => {
            panic!("zenoh get during partition returned error: {err}");
        }
    }

    // Heal: remove the iptables rules
    peers[0].allow_all_traffic(&network_name).await;
    peers[1].allow_all_traffic(&network_name).await;

    // Wait for peers to reconnect
    let result = wait_for_peer_connectivity(
        &sidecar,
        &peer_1_endpoint,
        &peer_2_endpoint,
        &peer_1_zid,
        &peer_2_zid,
        std::time::Duration::from_mins(1),
    )
    .await;
    if let Err(message) = result {
        panic!("peers did not reconnect after healing: {message}");
    }

    // Get after heal — should reach peer-2's queryable again
    let result = retry_zenoh_get(
        &sidecar,
        &peer_1_endpoint,
        "tests/c3",
        1000,
        1,
        std::time::Duration::from_secs(30),
    )
    .await;

    if let Err(message) = result {
        panic!("get after healing failed: {message}");
    }

    compose.down().await;
}

/// Retry a zenoh get until at least `min_replies` are received or the deadline elapses.
async fn retry_zenoh_get(
    sidecar: &Sidecar<'_>,
    endpoint: &str,
    key_expr: &str,
    timeout_ms: u64,
    min_replies: usize,
    deadline_duration: std::time::Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + deadline_duration;

    loop {
        let last_count = match sidecar
            .zenoh_get(endpoint, ZenohMode::Client, key_expr, timeout_ms)
            .await
        {
            Ok(count) if count >= min_replies => return Ok(()),
            Ok(count) => count,
            Err(err) => return Err(format!("zenoh get returned error: {err}")),
        };

        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for {min_replies} replies to `{key_expr}` (last: {last_count})"
            ));
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}

async fn wait_for_peer_disconnection(
    sidecar: &Sidecar<'_>,
    peer_1_endpoint: &str,
    peer_2_endpoint: &str,
    peer_1_zid: &str,
    peer_2_zid: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;

    loop {
        let last_reason = match peers_connected(
            sidecar,
            peer_1_endpoint,
            peer_2_endpoint,
            peer_1_zid,
            peer_2_zid,
        )
        .await
        {
            Ok(false) => return Ok(()),
            Ok(true) => "peers still see each other".to_owned(),
            Err(reason) => reason,
        };

        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for peers to disconnect: {last_reason}"
            ));
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}

async fn wait_for_peer_connectivity(
    sidecar: &Sidecar<'_>,
    peer_1_endpoint: &str,
    peer_2_endpoint: &str,
    peer_1_zid: &str,
    peer_2_zid: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;

    // Discovery, session establishment and introspection propagation all happen asynchronously
    // after the containers come up, so every step here is retried until the peers see each other
    // or the deadline elapses. The most recent reason for not yet being connected is reported on
    // timeout.
    let mut last_reason;

    loop {
        match peers_connected(
            sidecar,
            peer_1_endpoint,
            peer_2_endpoint,
            peer_1_zid,
            peer_2_zid,
        )
        .await
        {
            Ok(true) => return Ok(()),
            Ok(false) => last_reason = "peers do not see each other yet".to_owned(),
            Err(reason) => last_reason = reason,
        }

        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for peers to discover each other: {last_reason}"
            ));
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
}

/// Performs a single connectivity probe: fetches both peers' swarm status and checks whether each
/// peer lists the other. Returns `Err` for transient failures (the status could not be fetched or
/// a peer has not reported its own status yet) so the caller can retry.
async fn peers_connected(
    sidecar: &Sidecar<'_>,
    peer_1_endpoint: &str,
    peer_2_endpoint: &str,
    peer_1_zid: &str,
    peer_2_zid: &str,
) -> Result<bool, String> {
    let statuses_1 = sidecar
        .swarm_status_with_mode(peer_1_endpoint, ZenohMode::Peer)
        .await
        .map_err(|err| format!("failed to fetch peer 1 swarm status: {err}"))?;
    let statuses_2 = sidecar
        .swarm_status_with_mode(peer_2_endpoint, ZenohMode::Peer)
        .await
        .map_err(|err| format!("failed to fetch peer 2 swarm status: {err}"))?;

    let status_1 = statuses_1
        .iter()
        .find(|status| status.id.to_string() == peer_1_zid)
        .ok_or_else(|| format!("peer 1 has not reported its own status yet (zid={peer_1_zid})"))?;
    let status_2 = statuses_2
        .iter()
        .find(|status| status.id.to_string() == peer_2_zid)
        .ok_or_else(|| format!("peer 2 has not reported its own status yet (zid={peer_2_zid})"))?;

    let peer_1_sees_2 = status_1.peers.contains(&status_2.id);
    let peer_2_sees_1 = status_2.peers.contains(&status_1.id);

    Ok(peer_1_sees_2 && peer_2_sees_1)
}
