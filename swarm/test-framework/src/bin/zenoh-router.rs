//! A zenoh router for the swarm backends that run one next to their runtimes (docker, SSH). Built
//! from the workspace, so its zenoh version is the runtimes' by construction, which a stock
//! `zenohd` would not guarantee.
//!
//! It runs zenoh's default router config (listening on `tcp/[::]:7447`) until it is killed. Its
//! one optional argument is the multicast group (`<address>:<port>`) it scouts on instead of the
//! default one, the group of the swarm it serves.

use zenoh::Wait;

fn main() {
    let mut args = std::env::args().skip(1);
    let multicast_group = args.next();
    assert!(
        args.next().is_none(),
        "usage: zenoh-router [<multicast group>]"
    );

    let mut config = zenoh::Config::default();
    config
        .set_mode(Some(zenoh::config::WhatAmI::Router))
        .expect("setting zenoh mode cannot fail");
    if let Some(group) = multicast_group {
        config
            .insert_json5(
                "scouting/multicast/address",
                &serde_json::json!(group).to_string(),
            )
            .unwrap_or_else(|err| panic!("`{group}` is no multicast group: {err}"));
    }
    let _session = zenoh::open(config)
        .wait()
        .expect("failed to open the zenoh router");
    loop {
        // `park` may return spuriously, so it is called again
        std::thread::park();
    }
}
