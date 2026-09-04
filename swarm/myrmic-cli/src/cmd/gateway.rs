use crate::args::Ctx;

use crate::utils;
use swarm::{Swarm, SwarmConfig};
use tokio::signal::ctrl_c;
use tokio::signal::unix::{SignalKind, signal};

#[derive(clap::Parser)]
pub struct Gateway {
    /// TCP port to bind the gateway on. Defaults to 8080.
    #[clap(short = 'p', long)]
    pub port: Option<u16>,

    /// Routing configuration (JSON): which cell is allowed to own each mount,
    /// and the OIDC settings guarding it.
    #[clap(long)]
    pub routing: Option<std::path::PathBuf>,

    /// Serve behind HTTPS, so session cookies are marked `Secure`.
    #[clap(long)]
    pub over_https: bool,

    /// How long a session may sit idle before it expires, e.g. `30s` or `2m`.
    #[clap(long)]
    pub session_inactivity: Option<humantime::Duration>,
}

pub async fn handle(ctx: Ctx, cmd: Gateway) -> anyhow::Result<()> {
    let Gateway {
        port,
        routing,
        over_https,
        session_inactivity,
    } = cmd;

    let mut config = SwarmConfig::default();
    config
        .zenoh
        .set_mode(Some(zenoh::config::WhatAmI::Peer))
        .expect("setting mode cannot fail here");
    config.plugins.gateway = Some(swarm_gateway::Config {
        port,
        over_https,
        session_inactivity_timer_secs: session_inactivity
            .map(|idle| i64::try_from(idle.as_secs()).unwrap_or(i64::MAX)),
        routing: routing.map(swarm_gateway::Either::Left),
    });
    // The gateway registers no exec, so network listings would show a bare id;
    // have the introspection plugin carry a self-description instead.
    config.plugins.introspection.participant = Some(introspection_client::v1::ParticipantInfo {
        kind: "gateway".to_owned(),
        name: utils::command_path(),
        origin: utils::origin(),
    });
    config.telemetry.logs.env_filter = utils::build_filter(ctx);

    let spawned = Swarm::new(config).wait_in_place().await?;

    shutdown_signal().await;

    spawned.kill_async().await;
    Ok(())
}

/// Resolves when the process receives SIGTERM or Ctrl+C.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = ctrl_c().await;
    };

    let terminate = async {
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            // No SIGTERM handler available; fall back to Ctrl+C only.
            Err(_) => std::future::pending::<()>().await,
        }
    };

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
