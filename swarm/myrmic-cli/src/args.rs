use crate::cmd::*;

#[derive(clap::Parser)]
#[command(name = "myrmic", version = env!("MYRMIC_VERSION"))]
pub struct Args {
    #[clap(flatten)]
    pub ctx: Ctx,

    #[clap(subcommand)]
    pub command: Command,
}

#[derive(clap::Parser, Default, Clone, Copy)]
pub struct Ctx {
    /// Increase logging verbosity.
    #[clap(short = 'v', long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Defines how long we should wait before giving up, e.g. `2s`, `500ms`, `1m30s`.
    #[clap(long, alias = "network_timeout", global = true)]
    pub timeout: Option<humantime::Duration>,
}

impl Ctx {
    /// Returns true if the given level should be emitted at this verbosity.
    pub fn is_enabled(self, level: crate::log::Level) -> bool {
        use crate::log::Level;
        match level {
            Level::Error | Level::Warn | Level::Info => true,
            Level::Debug => self.verbose >= 1,
            Level::Trace => self.verbose >= 2,
        }
    }

    pub fn sorg(self, session: zenoh::Session) -> sorg_client::Client {
        let mut config = sorg_client::Config::default();

        if let Some(timeout) = self.timeout {
            config.set_query_timeout(timeout.into());
        }

        sorg_client::Client::new_with_config(session, config)
    }

    pub async fn introspection(self, session: zenoh::Session) -> introspection_client::v1::Client {
        introspection_client::v1::Client::new(session).await
    }
}

#[derive(clap::Subcommand)]
pub enum Command {
    // Project
    /// Scaffold a new cell crate from a template.
    ///
    /// Creates a fresh Rust crate wired up against peeriot's `myrmic_sdk`, ready to build.
    New(new::New),
    /// Build a cell or an application suite.
    ///
    /// Compiles the cell to the provided platform (default is `linux`)
    /// Can also be used to generate an api file to provide external parties your cell's API.
    ///
    /// If compiling an `app_specs.yml`, then all artifacts will be bundled into a `nest` archive.
    Build(build::Build),

    // Management
    #[clap(alias = "db")]
    Database(database::Database),
    /// Configure which nodes replicate which data.
    ///
    /// An entry pairs something to replicate — an application, a cell, or a
    /// scope — with the tags of the nodes that should hold a replica. Nodes
    /// match those tags against their own, so configuration never names a node.
    ///
    /// With no arguments, lists every configured replication set.
    #[clap(alias = "replicas", alias = "replica", alias = "rep")]
    Replicate(replicate::Replicate),
    /// Add and remove tags on nodes.
    ///
    /// A node's tags decide both which cells it may run and which data it
    /// replicates. Its configuration supplies the tags it starts with; these
    /// are carried on top, and reach a running node without a restart.
    ///
    /// With no arguments, lists every node and its tags.
    #[clap(alias = "tag")]
    Tags(tags::Tags),
    /// Send a command to a running cell.
    ///
    /// Delivers a command to the cell addressed by the given SRI or SRN.
    #[clap(alias = "command", alias = "cmd")]
    Send(send::Send),
    /// Send an event into the myrmic network.
    #[clap(alias = "event", alias = "pub")]
    Publish(publish::Publish),
    /// Subscribe to events and log them as they arrive.
    ///
    /// With no arguments, logs every event on the network. Given one or more
    /// event names (space- or comma-separated), only those events are logged.
    /// Runs until interrupted.
    #[clap(alias = "sub")]
    Subscribe(subscribe::Subscribe),
    /// Deploy a built cell to myrmic.
    ///
    /// Uploads and starts a cell on the network.
    Deploy(deploy::Deploy),
    /// Run the socket gateway.
    ///
    /// Binds an HTTP/WebSocket entrypoint into the swarm on the given port,
    /// serving deployed web applications (static assets from the blob store
    /// plus a cell command/event API).
    Gateway(gateway::Gateway),
    /// Remove a deployed cell or application.
    ///
    /// Interactively asks what to delete — the app the target belongs to, just
    /// the cell, the cell with its descendants, or only the descendants —
    /// offering only the choices that make sense for the target (default:
    /// nothing). Pass --app / --cell / --branch / --children to choose
    /// non-interactively and skip the prompt. A bare UUID is always a cell.
    #[clap(alias = "rm", alias = "stop")]
    Delete(delete::Delete),
    /// Inspect and manage cells on myrmic.
    ///
    /// Subcommands manage cell classes, show cell status, and tear down cells.
    /// With no subcommand, lists registered cells.
    #[clap(alias = "cell")]
    Cells(cells::Cells),
    /// Manage local myrmic runtimes.
    ///
    /// Subcommands list, start, and stop runtime instances.
    /// With no subcommand, lists known runtimes.
    #[clap(alias = "runtime", alias = "rt")]
    Runtimes(runtimes::Runtimes),
    #[clap(alias = "nodes")]
    Network(network::Network),

    /// Query telemetry data.
    ///
    /// Subcommands logs, and traces.
    #[cfg(feature = "telemetry")]
    Telemetry(telemetry::Telemetry),

    /// List the build platforms supported by this CLI.
    ///
    /// Prints each triple.
    Platforms(platforms::Platforms),
}
