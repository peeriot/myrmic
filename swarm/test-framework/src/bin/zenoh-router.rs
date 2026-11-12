//! A zenoh router for test setups that run one next to their runtimes (e.g. on a docker network),
//! as a member of the setup's overlay (see `test_framework::overlay`). Built from the workspace,
//! so its zenoh version is the runtimes' by construction, which a stock `zenohd` would not
//! guarantee.
//!
//! Its one argument is the `zenoh` section of its config as JSON, as the overlay gives it to every
//! member: the group it scouts on, the port it listens on, the members it connects to. It prints `ready` on stdout once it listens, and runs until it is killed.

use zenoh::Wait;

fn main() {
    let mut args = std::env::args().skip(1);
    let settings = args
        .next()
        .expect("usage: zenoh-router <zenoh settings as JSON>");
    assert!(
        args.next().is_none(),
        "usage: zenoh-router <zenoh settings as JSON>"
    );

    let mut config = zenoh::Config::from_json5(&settings)
        .unwrap_or_else(|err| panic!("`{settings}` is no zenoh config: {err}"));
    config
        .set_mode(Some(zenoh::config::WhatAmI::Router))
        .expect("setting zenoh mode cannot fail");
    let _session = zenoh::open(config)
        .wait()
        .expect("failed to open the zenoh router");
    println!("ready");
    loop {
        // `park` may return spuriously, so it is called again
        std::thread::park();
    }
}
