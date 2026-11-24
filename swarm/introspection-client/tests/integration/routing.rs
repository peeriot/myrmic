use std::time::Duration;

use sorg_tests::enable_test_logging;
use swarm::{Swarm, SwarmConfig};
use zenoh::{Session, config::ZenohId};

use crate::integration::wait_for;

const KEY: &str = "test/nonclique/ping";

/// A - R - B with A and B only connected to R: data from A must reach B through R.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_crosses_a_router_between_unconnected_runtimes() {
    enable_test_logging("debug");

    // Arrange - R listens on an OS-chosen port, A and B only connect to R
    let r = start_runtime("tcp/127.0.0.1:0", &[]).await;
    let r_locators: Vec<String> = r
        .session()
        .info()
        .locators()
        .await
        .into_iter()
        .map(|locator| locator.to_string())
        .filter(|locator| locator.starts_with("tcp/127.0.0.1:"))
        .collect();
    assert!(!r_locators.is_empty(), "R has no loopback locator");
    let a = start_runtime("tcp/127.0.0.1:0", &r_locators).await;
    let b = start_runtime("tcp/127.0.0.1:0", &r_locators).await;
    let (r_id, a_id, b_id) = (r.session().zid(), a.session().zid(), b.session().zid());

    // Assert - both are linked to R and not to each other
    wait_for("A and B to be linked to R", || async {
        let a_links = linked_zids(a.session()).await;
        let b_links = linked_zids(b.session()).await;
        (a_links.contains(&r_id) && b_links.contains(&r_id)).then_some(())
    })
    .await;
    assert!(!linked_zids(a.session()).await.contains(&b_id));
    assert!(!linked_zids(b.session()).await.contains(&a_id));

    // Act - B subscribes, A keeps publishing until the subscription is known to A
    let subscriber = b
        .session()
        .declare_subscriber(KEY)
        .await
        .expect("failed to declare subscriber on B");
    let received = wait_for("data from A to arrive at B", || async {
        a.session()
            .put(KEY, "ping")
            .await
            .expect("failed to publish on A");
        tokio::time::timeout(Duration::from_millis(200), subscriber.recv_async())
            .await
            .ok()
            .and_then(Result::ok)
    })
    .await;

    // Assert - the sample is the one A sent
    assert_eq!("ping", received.payload().try_to_string().unwrap());
}

/// Starts an in-process runtime that leaves zenoh's `mode` unset, with all
/// scouting off so it only talks to the endpoints it is given.
async fn start_runtime(listen: &str, connect: &[String]) -> swarm::spawn::Spawned {
    let mut config = SwarmConfig::default();
    let zenoh = &mut config.zenoh;
    zenoh
        .insert_json5("scouting/multicast/enabled", "false")
        .expect("failed to disable multicast scouting");
    zenoh
        .insert_json5("scouting/gossip/enabled", "false")
        .expect("failed to disable gossip scouting");
    zenoh
        .insert_json5("listen/endpoints", &format!("[\"{listen}\"]"))
        .expect("failed to set listen endpoints");
    zenoh
        .insert_json5(
            "connect/endpoints",
            &serde_json::to_string(connect).expect("failed to encode connect endpoints"),
        )
        .expect("failed to set connect endpoints");

    Swarm::new(config)
        .wait_in_place()
        .await
        .expect("failed to start runtime")
}

async fn linked_zids(session: &Session) -> Vec<ZenohId> {
    let info = session.info();
    info.routers_zid()
        .await
        .chain(info.peers_zid().await)
        .collect()
}
