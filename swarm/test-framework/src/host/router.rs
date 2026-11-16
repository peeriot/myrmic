use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use zenoh::config::WhatAmI;

use super::Remote;
use crate::{overlay, run_cleanup};

/// A standalone zenoh router on a host, a member of an overlay that is no runtime: nothing starts
/// or stops it but its creation (e.g. [`super::Docker::router`]) and its drop, and no swarm
/// counts it among its runtimes. A swarm attaches through one by a connect to it
/// (`overlay.connect_to(&router)`), so faults on the runtimes never cut the test off.
///
/// It runs the `zenoh-router` binary with the overlay's settings for its member: the group it
/// scouts on, and a port of its own it listens on for what connects to it.
pub struct Router<H: Remote> {
    member: overlay::Member,
    /// the host it runs on
    host: H,
    /// the router's pid on its host
    pid: String,
    /// the local process (`docker exec`, `ssh`) the router runs in
    _process: tokio::process::Child,
    /// records the router as running in its overlay while it exists; dropped after the router is
    /// killed in [`Drop::drop`]
    _running: overlay::RunningGuard,
}

impl<H: Remote> Router<H> {
    /// Start the router `member` on `host`, running the `zenoh-router` binary at `binary` there;
    /// returns once it listens.
    pub(crate) async fn start(host: H, binary: &str, member: overlay::Member) -> Self {
        member.ensure_listen_port();
        let settings = member.get_zenoh_settings().to_string();
        // `echo $$` first: the router's pid on its host, which the drop kills
        let command = host.command(
            "sh",
            &["-c", "echo $$; exec \"$@\"", "sh", binary, &settings],
        );
        let mut process = tokio::process::Command::from(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to run the router's command");
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

    /// the host the router runs on, e.g. for its faults
    pub fn get_host(&self) -> &H {
        &self.host
    }
}

impl<H: Remote> From<&Router<H>> for overlay::Member {
    fn from(router: &Router<H>) -> Self {
        router.member.clone()
    }
}

impl<H: Remote> Drop for Router<H> {
    fn drop(&mut self) {
        // ending the local process doesn't end what it runs on the host
        run_cleanup("Router", self.host.command("kill", &[&self.pid]));
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
