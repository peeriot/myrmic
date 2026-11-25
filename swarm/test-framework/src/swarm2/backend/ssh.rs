use std::net::TcpListener;
use std::process::{Child, Stdio};

use enum_map::{Enum, EnumMap};

use super::{ROUTER_PORT, router_session};
use crate::{
    myrmic::{self, Myrmic, backend::isolated_multicast_group},
    ssh_utils::RemoteBinary,
    swarm2::{SwarmBackend, SwarmSpec},
};

/// [`SwarmBackend`] that runs every runtime on a remote host over SSH, next to a zenoh router on
/// an entry host the test reaches through an SSH tunnel.
///
/// The swarm is isolated like [`super::local::Local`]'s: each runtime has state directories of
/// its own on its host, removed with the swarm, and the runtimes and the router scout on a
/// multicast group of the swarm's own. So a runtime sees nothing of earlier or other swarms on
/// the same hosts, its name and data included, and keeps its data across restarts within the
/// swarm.
pub struct Ssh<S: Enum> {
    /// the CLI on each runtime's host, in the runtime's isolation
    shims: EnumMap<S, Myrmic<myrmic::backend::Ssh>>,
    /// `ssh -tt` running `zenoh-router` on the entry host, killed on drop
    router: Child,
    /// `ssh -N -L` from [`Self::tunnel_endpoint`] to the router on the entry host, killed on drop
    tunnel: Child,
    /// the local end of the tunnel, e.g. `tcp/127.0.0.1:41234`
    tunnel_endpoint: String,
    /// the CLI on the test's host, connected to the router through the tunnel
    client: Myrmic<myrmic::Local>,
}

impl<S: Enum> Ssh<S> {
    /// Creates each runtime's isolation on its host, starts `zenoh-router` (at `client_entry`'s
    /// binary path) on `client_entry` and opens an SSH tunnel from a free local port to its port.
    /// Returns once both `ssh` processes are spawned, not once they are connected.
    pub fn new(remotes: EnumMap<S, RemoteBinary>, client_entry: &RemoteBinary) -> Self {
        let multicast_group = isolated_multicast_group();
        let shims = remotes.map(|_, remote| {
            Myrmic::ssh_isolated(
                remote.login,
                remote.remote_bin_path,
                multicast_group.clone(),
            )
        });
        // `-tt` gives the router a terminal on the remote side: when this `ssh` dies, however it
        // dies, the terminal hangs up and the router gets SIGHUP, so no router outlives the test.
        // stdin is a pipe that stays open as long as `router` lives: an EOF on it could end the
        // remote terminal session early.
        let router = crate::ssh_utils::command_with_ssh_config("ssh")
            .args(["-tt", "-o", "BatchMode=yes"])
            .arg(&client_entry.login)
            .arg(crate::ssh_utils::quoted_command_line(&[
                &client_entry.remote_bin_path,
                &multicast_group,
            ]))
            .stdin(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| {
                panic!(
                    "failed to spawn zenoh-router on {} over ssh: {e}",
                    client_entry.login
                )
            });
        // bind then drop releases the port; another process may take it before `ssh` binds it,
        // which `ExitOnForwardFailure` turns into an `ssh` that exits instead of one without a
        // forward
        let tunnel_port = TcpListener::bind("127.0.0.1:0")
            .expect("failed to bind an ephemeral port")
            .local_addr()
            .expect("a bound listener has an address")
            .port();
        let tunnel = crate::ssh_utils::command_with_ssh_config("ssh")
            .args([
                "-N",
                "-o",
                "BatchMode=yes",
                "-o",
                "ExitOnForwardFailure=yes",
            ])
            .arg("-L")
            .arg(format!("{tunnel_port}:127.0.0.1:{ROUTER_PORT}"))
            .arg(&client_entry.login)
            .stdin(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| {
                panic!(
                    "failed to spawn the ssh tunnel to {}: {e}",
                    client_entry.login
                )
            });
        let tunnel_endpoint = format!("tcp/127.0.0.1:{tunnel_port}");
        Self {
            shims,
            router,
            tunnel,
            client: Myrmic::local_connected(tunnel_endpoint.clone()),
            tunnel_endpoint,
        }
    }
}

impl<S: Enum> Drop for Ssh<S> {
    fn drop(&mut self) {
        // `kill` is `Ok` for an `ssh` that already exited, e.g. because its connection dropped
        self.router
            .kill()
            .expect("failed to kill the ssh running zenoh-router");
        self.router
            .wait()
            .expect("failed to reap the ssh running zenoh-router");
        self.tunnel.kill().expect("failed to kill the ssh tunnel");
        self.tunnel.wait().expect("failed to reap the ssh tunnel");
    }
}

// The bound makes `Ssh<S>` `Sync`, which the `Send` future of `connect_session` borrowing it
// needs; the compiler cannot see through `S::Array` to the array it is.
impl<S: SwarmSpec> SwarmBackend<S> for Ssh<S>
where
    S::Array<Myrmic<myrmic::backend::Ssh>>: Sync,
{
    type Cli = myrmic::backend::Ssh;

    type Client = myrmic::backend::Local;

    /// the runtime's isolated CLI, the same one across restarts
    fn shim(&self, runtime: S) -> crate::myrmic::Myrmic<Self::Cli> {
        self.shims[runtime].clone()
    }

    fn client(&self) -> crate::myrmic::Myrmic<Self::Client> {
        self.client.clone()
    }

    /// a client session on the router, through the tunnel
    async fn connect_session(&self) -> zenoh::Session {
        router_session(&self.tunnel_endpoint).await
    }
}
