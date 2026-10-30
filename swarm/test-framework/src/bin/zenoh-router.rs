//! A zenoh router for the docker swarm backend's containers. Built from the workspace, so its
//! zenoh version is the runtimes' by construction, which a stock `zenohd` would not guarantee.
//!
//! It runs zenoh's default router config (listening on `tcp/[::]:7447`) until it is killed.

use zenoh::Wait;

fn main() {
    let mut config = zenoh::Config::default();
    config
        .set_mode(Some(zenoh::config::WhatAmI::Router))
        .expect("setting zenoh mode cannot fail");
    let _session = zenoh::open(config)
        .wait()
        .expect("failed to open the zenoh router");
    loop {
        // `park` may return spuriously, so it is called again
        std::thread::park();
    }
}
