//! The `replication_delay` test hook (`swarm::TestHooks`): a node that holds back what replication
//! brings in.

use std::time::{Duration, Instant};

use super::{
    TABLE, boot, commit_and_await_version, holding, node_id, open_session_with, replicate_onto,
    scope, start_node_on,
};
use db_client::v1::Client;
use db_commons::models::Subject;

/// how long the held back node holds back replication
const DELAY: Duration = Duration::from_secs(3);

/// How long a held back commit may take to arrive beyond [`DELAY`]: a lagging copy catches up
/// once the delay is over, not seconds later.
const ARRIVAL_SLACK: Duration = Duration::from_secs(10);

/// A node whose test hooks hold back replication holds a commit made on another node only once
/// the delay has passed.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_back_node_holds_a_commit_only_after_its_delay() -> anyhow::Result<()> {
    // Bound and released to find a free port; a bind racing another process for it is accepted
    // for a test.
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let endpoints = format!(r#"["tcp/127.0.0.1:{port}"]"#);
    // A transaction without a constraint lands on the node with the highest id, so the writer
    // gets the highest id there is, and the commit lands on it.
    let (writer, _drop_writer) = start_node_on(
        |zenoh| {
            zenoh
                .insert_json5("id", r#""ffffffffffffffffffffffffffffffff""#)
                .expect("a zenoh id is valid json5");
            zenoh
                .insert_json5("listen/endpoints", &endpoints)
                .expect("endpoints are valid json5");
        },
        Default::default(),
    )
    .await;
    let held_back = open_session_with(|zenoh| {
        zenoh
            .insert_json5("id", r#""1""#)
            .expect("a zenoh id is valid json5");
        zenoh
            .insert_json5("connect/endpoints", &endpoints)
            .expect("endpoints are valid json5");
    })
    .await;
    let (_drop_held_back, drop_rx) = flume::bounded(1);
    let plugins = crate::config::PluginConfigs {
        test_hooks: crate::TestHooks {
            replication_delay: DELAY,
        },
        ..Default::default()
    };
    boot(&held_back, drop_rx, Default::default(), plugins).await;

    let client = Client::new(&writer);
    replicate_onto(&client, &scope(), &[&writer, &held_back]).await;
    let replica = db_client::replica_v1::Client::new(&writer, &Subject::Scope(scope()))
        .map_err(anyhow::Error::from_boxed)?;
    // The held back node replicates the scope once it has heard of the configuration, itself
    // held back; only then does a commit's lag show what the delay does to a running copy.
    let configured = Instant::now();
    while !holding_any(&replica, &held_back).await? {
        anyhow::ensure!(
            configured.elapsed() < DELAY + ARRIVAL_SLACK,
            "the held back node never replicated the scope"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (events_tx, events_rx) = flume::unbounded();
    let _sub = client
        .subscribe(Subject::Scope(scope()), TABLE, move |notification| {
            events_tx
                .send(notification)
                .expect("the test still listens");
        })
        .await
        .map_err(anyhow::Error::from_boxed)?;
    // before the commit: the held back node may get it before the test sees its event
    let committing = Instant::now();
    let version = commit_and_await_version(&client, &events_rx).await;
    anyhow::ensure!(
        holding(&replica, &scope(), version)
            .await
            .contains(&node_id(&writer)),
        "the commit didn't land on the writer"
    );

    while !holding(&replica, &scope(), version)
        .await
        .contains(&node_id(&held_back))
    {
        anyhow::ensure!(
            committing.elapsed() < DELAY + ARRIVAL_SLACK,
            "the held back node never held the commit"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let arrival = committing.elapsed();
    anyhow::ensure!(
        arrival >= DELAY,
        "the held back node held the commit after {arrival:?}, before its delay of {DELAY:?}"
    );
    Ok(())
}

/// whether `node` holds the scope at any version
async fn holding_any(
    replica: &db_client::replica_v1::Client,
    node: &zenoh::Session,
) -> anyhow::Result<bool> {
    let holders = replica
        .locate(&scope(), None)
        .await
        .map_err(anyhow::Error::from_boxed)?;
    Ok(holders.iter().any(|holder| holder.id == node_id(node)))
}
