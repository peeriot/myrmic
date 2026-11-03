//! `myrmic runtimes start`, through [`test_framework::myrmic`]. They need a built `myrmic` binary
//! (see [`test_framework::resolve_binary!`]).

use test_framework::myrmic::Myrmic;

/// With multicast scouting off, a runtime started with `--connect` to another's `--listen`
/// endpoint still links to it.
#[tokio::test(flavor = "multi_thread")]
async fn connect_and_listen_flags_join_runtimes_without_multicast() {
    let myrmic = Myrmic::local();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("no-multicast.yaml");
    // JSON is YAML, which `runtimes start` parses
    let content = serde_json::json!({
        "zenoh": { "scouting": { "multicast": { "enabled": false }, "gossip": { "enabled": false } } },
    });
    std::fs::write(&config, content.to_string()).unwrap();
    let config = config.to_str().unwrap();
    let endpoint = format!("tcp/127.0.0.1:{}", free_port());

    let listener = myrmic
        .runtime(&[])
        .config(config)
        .listen(&endpoint)
        .start()
        .await
        .unwrap();
    let joiner = myrmic
        .runtime(&[])
        .config(config)
        .connect(&endpoint)
        .start()
        .await
        .unwrap();

    // peers don't relay queries, and gossip is off: only the listener answers this session
    let config = Myrmic::local_connected(&endpoint).backend().zenoh_config();
    let session = zenoh::open(config).await.unwrap();
    let statuses = introspection_client::v1::Client::new(session)
        .await
        .swarm_status()
        .await
        .unwrap();
    let listener_status = statuses
        .iter()
        .find(|status| status.id == listener.zid())
        .expect("the listener reported no status");
    assert!(
        listener_status.peers.contains(&joiner.zid()),
        "the joiner isn't linked to the listener: {statuses:?}"
    );
}

/// a TCP port nothing listens on right now
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
