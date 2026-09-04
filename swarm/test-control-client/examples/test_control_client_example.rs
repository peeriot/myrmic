//! # Test Control Client Example
//!
//! This example demonstrates how to interact with the **Test Control plugin**
//! using the `test_control_client` crate.
//!
//! It performs a complete flow:
//! 1. Deploys a local Zenoh swarm with the Test Control plugins.
//! 2. Creates a Zenoh session and instantiates a `Client`.
//! 3. Sends various Test Control commands (health check, subscriber/publisher creation, introspection).
//! 4. Verifies that messages are successfully sent and received.
//! 5. Queries introspection data from the Test Control plugin and prints node information.
//!
//! ## Usage
//! - **Build the workspace first** to ensure all plugin binaries are available, from the workspace root run:
//!   ```bash
//!   .ci/build/build
//!   ```
//!
//! - **Run the example** from the workspace root:
//!   ```bash
//!   cargo run --package test-control-client --example test_control_client_example
//!   ```
//!
//! ## Notes
//! - Uses `examples/data/swarm-config.jsonnet` for swarm configuration.

use std::{env, time::Duration};

use sorg_tests::set_up_swarm_with_config;
use test_control_client::Client;
use test_control_common::Reply;
use zenoh::{Config, config::WhatAmI};

const PATH_SWARM_CONFIG: &str = "examples/data/swarm-config.jsonnet";

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let _swarm_handle = set_up_swarm_with_config(&swarm_config()).await;
    println!("deployed test control plugins");

    let mut z_config = Config::default();
    z_config
        .set_mode(Some(WhatAmI::Peer))
        .expect("failed to set role");
    let session = zenoh::open(z_config)
        .await
        .expect("failed to open zenoh session");

    let tc_client = Client::new(session);
    println!("test control client created");

    // This id the sane one set inside the `swarm-config.jsonnet`
    // to make sure that the example targets the right node.
    let zid = String::from("9a28d1a01a04f41e7b9e0ff3bab594a2");

    let health = tc_client
        .health(zid.clone())
        .await
        .expect("failed to query health");

    if let Reply::Health { .. } = health {
        println!("{:?}", health);
    } else {
        println!("did not get any reply");
    }

    let subscriber = tc_client
        .create_subscriber(
            zid.clone(),
            String::from("examples/discovery"),
            Some(5),
            None,
        )
        .await
        .expect("failed to create subscriber");

    if let Reply::SubscriberCreated { .. } = subscriber {
        println!("{:?}", subscriber);
    } else {
        println!("did not get any reply");
    }

    let publisher = tc_client
        .create_publisher(
            zid.clone(),
            String::from("examples/discovery"),
            String::from("hello"),
            Some(5),
            Some(Duration::from_millis(100)),
        )
        .await
        .expect("failed to create publisher");

    if let Reply::PublisherCreated { .. } = publisher {
        println!("{:?}", publisher);
    } else {
        println!("did not get any reply");
    }

    tokio::time::sleep(Duration::from_secs(5)).await;

    let stats = tc_client
        .stats(zid.clone(), String::from("examples/discovery"))
        .await
        .expect("failed to get stats");

    if let Reply::Stats { sent, received, .. } = stats {
        assert_eq!(sent, 5);
        assert_eq!(received, 5);
        println!("{:?}", stats);
    } else {
        println!("did not get any reply");
    }

    let nodes_status = tc_client
        .introspection(zid)
        .await
        .expect("failed to get introspection");

    if let Reply::Introspection { nodes_status, .. } = nodes_status {
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
    } else {
        println!("did not get any reply");
    }
}

fn swarm_config() -> String {
    format!("{cargo_dir}/{PATH_SWARM_CONFIG}", cargo_dir = cargo_dir())
}

fn cargo_dir() -> String {
    env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set by Cargo")
}
