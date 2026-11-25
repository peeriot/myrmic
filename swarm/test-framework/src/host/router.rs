use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use zenoh::config::WhatAmI;

use super::Remote;
use crate::{overlay, run_cleanup};

/// A standalone zenoh router on a host. It is a member of an overlay, but not a runtime: it starts
/// when it is created (e.g. by [`super::Docker::router`]) and stops when it is dropped, and no
/// swarm counts it among its runtimes. A swarm can attach through a router by a connect to it
/// (`overlay.connect_to(&router)`). Then faults on the runtimes never cut the test off from the
/// swarm.
///
/// It runs the `zenoh-router` binary with the overlay's settings for it: the group it scouts on,
/// and a port of its own on which it listens for connects.
pub struct Router<H: Remote> {
    /// records the router as running in its overlay while it exists; dropped after the router is
    /// killed in [`Drop::drop`], and before `host`, which may be the last handle to the host the
    /// overlay reads while it counts the router running
    _up: overlay::UpGuard,
    member: overlay::Member,
    /// the host it runs on
    host: H,
    /// the router's pid on its host
    pid: String,
    /// the local process (`docker exec`, `ssh`) the router runs in
    _process: tokio::process::Child,
}

impl<H: Remote> Router<H> {
    /// Start the router `member` on `host`, running the `zenoh-router` binary at `binary` there;
    /// returns once it listens.
    pub(super) async fn start(host: H, binary: &str, member: overlay::Member) -> Self {
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
        let pid = next_line(&mut lines, "the router's pid")
            .await
            .unwrap_or_else(|err| panic!("{err}"));
        let ready = next_line(&mut lines, "the router to be ready").await;
        if ready.as_deref() != Ok("ready") {
            // there is no `Router` yet whose drop would end it
            run_cleanup("Router", host.command("kill", &[&pid]));
            panic!("the router didn't become ready: {ready:?}");
        }
        Self {
            _up: member.come_up(WhatAmI::Router),
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

/// The next line of `lines`, a process's output, which has to come within the default timeout; or
/// why there is none, naming `what` the line was to tell.
async fn next_line(
    lines: &mut Lines<BufReader<tokio::process::ChildStdout>>,
    what: &str,
) -> Result<String, String> {
    match tokio::time::timeout(crate::wait::DEFAULT_TIMEOUT, lines.next_line()).await {
        Ok(Ok(Some(line))) => Ok(line),
        Ok(Ok(None)) => Err(format!("the process ended before {what}")),
        Ok(Err(err)) => Err(format!("failed to read {what}: {err}")),
        Err(_) => Err(format!("timed out waiting for {what}")),
    }
}
