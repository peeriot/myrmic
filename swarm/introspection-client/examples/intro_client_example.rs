//! Example demonstrating the interaction with the introspection client via the introspection-client
//! NOTE: Since the example uses the swarm lib to spin up the sorg runtimes, you should
//! (a) make sure to build the whole workspace first (to build the plugin binaries)
//! (b) run the example from the workspace root directory by running `RUST_LOG=off cargo run --package introspection-client --example usage`
//! running `cargo run --example usage` will fail since swarm will fail to find the plugin binaries.

use std::env;

use introspection_client::v1::Client;
use sorg_tests::set_up_swarm_with_config;
use zenoh::{Config, config::WhatAmI};

#[tokio::main]
async fn main() {
    let _swarm_handle_one =
        set_up_swarm_with_config(&config("examples/data/three_nodes_a.jsonnet")).await;
    let _swarm_handle_two =
        set_up_swarm_with_config(&config("examples/data/three_nodes_b.jsonnet")).await;
    let _swarm_handle_three =
        set_up_swarm_with_config(&config("examples/data/three_nodes_c.jsonnet")).await;
    println!("deployed swarm nodes");

    // to create the client, you need to set up a zenoh session which can connect to the sorg-orch runtimes you want to interact with
    // (alternatively, you can attach the client to an existing zenoh session)
    let mut z_config = Config::default();
    z_config
        .set_mode(Some(WhatAmI::Peer))
        .expect("failed to set role");
    let session = zenoh::open(z_config)
        .await
        .expect("failed to open zenoh session");

    let client = Client::new(session).await;

    let nodes_status = client
        .swarm_status()
        .await
        .expect("failed to get swarm status");

    for node_status in nodes_status {
        println!("Zenoh ID of node: {id}", id = node_status.id);
        println!("Peers connected to the node:");
        for peer_id in node_status.peers {
            println!("{peer_id}");
        }
        for router_id in node_status.routers {
            println!("{router_id}");
        }
        println!("Plugins hosted on the node:");
        for plugin in node_status.plugins {
            println!(
                "Name: {name}; Config: {config}",
                name = plugin.name,
                config = plugin.config
            );
        }
        println!();
    }
}

fn config(path: &str) -> String {
    format!("{}/{}", cargo_dir(), path)
}

fn cargo_dir() -> String {
    env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set by Cargo")
}
