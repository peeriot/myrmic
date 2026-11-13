use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader, Lines};

use super::run_cleanup;
use zenoh::config::WhatAmI;

use crate::{host, overlay};

/// A standalone zenoh router in a docker container, a member of an overlay that is no runtime:
/// nothing starts or stops it but its creation ([`crate::host::Docker::router`]) and its drop, and
/// no swarm counts it among its runtimes. A swarm attaches through one by a connect to it
/// (`overlay.connect_to(&router)`), so faults on the runtimes never cut the test off.
///
/// It runs the `zenoh-router` binary with the overlay's settings for its member: the group it
/// scouts on, and a port of its own it listens on for what connects to it.
pub struct Router {
    member: overlay::Member,
    /// the host whose container it runs in
    host: host::Docker,
    /// the router's pid in its container
    pid: String,
    /// the `docker exec` the router runs in
    _process: tokio::process::Child,
    /// records the router as running in its overlay while it exists; dropped after the router is
    /// killed in [`Drop::drop`]
    _running: overlay::RunningGuard,
}

impl Router {
    /// Start the router `member` in `host`'s container; returns once it listens.
    pub(crate) async fn start(host: host::Docker, member: overlay::Member) -> Self {
        member.ensure_listen_port();
        let container = host.get_container();
        let settings = member.get_zenoh_settings().to_string();
        // `echo $$` first: the router's pid in the container, which the drop kills
        let mut command = container.docker_cli();
        command.args([
            "exec",
            container.id(),
            "sh",
            "-c",
            "echo $$; exec \"$@\"",
            "sh",
            "zenoh-router",
            &settings,
        ]);
        let mut process = tokio::process::Command::from(command)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to run docker exec for the router");
        let mut lines = BufReader::new(process.stdout.take().expect("stdout is piped")).lines();
        let pid = next_line(&mut lines, "the router's pid").await;
        let ready = next_line(&mut lines, "the router to be ready").await;
        assert_eq!(
            ready, "ready",
            "the router said something else than `ready`"
        );
        Self {
            _running: member.start_running(WhatAmI::Router),
            member,
            host,
            pid,
            _process: process,
        }
    }
}

impl Router {
    /// the host the router runs on, e.g. for its network's faults
    pub fn get_host(&self) -> &host::Docker {
        &self.host
    }
}

impl From<&Router> for overlay::Member {
    fn from(router: &Router) -> Self {
        router.member.clone()
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        // ending `docker exec` doesn't end what it runs in the container
        let container = self.host.get_container();
        let mut kill = container.docker_cli();
        kill.args(["exec", container.id(), "kill", &self.pid]);
        run_cleanup("Router", kill);
    }
}

/// the next line of `lines`, a process's output, which has to come within the default timeout
async fn next_line(
    lines: &mut Lines<BufReader<tokio::process::ChildStdout>>,
    what: &str,
) -> String {
    tokio::time::timeout(crate::wait::DEFAULT_TIMEOUT, lines.next_line())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap_or_else(|err| panic!("failed to read {what}: {err}"))
        .unwrap_or_else(|| panic!("the process ended before {what}"))
}
