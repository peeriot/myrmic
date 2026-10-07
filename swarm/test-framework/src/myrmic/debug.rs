//! Streaming `myrmic telemetry debug --json` on any [`MyrmicBackend`].

use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize as _;
use swarm_telemetry::db::opentelemetry_proto::tonic::logs::v1::LogRecord;
use swarm_telemetry::debug::{DebugCommand, DebugEvent};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::ChildStdout;

use super::MyrmicBackend;

/// A running `myrmic telemetry debug`, read line by line.
///
/// Dropping it interrupts the CLI with `SIGINT`, not `SIGKILL`: only on Ctrl-C does the CLI put
/// back a log filter it raised for `--level` on the mesh's nodes. The drop then waits for the CLI
/// to exit.
pub struct DebugListener {
    // Declared before `process`, so it drops (closing the pipe) between the interrupt and the
    // wait; see the `Drop` impl.
    lines: Lines<BufReader<ChildStdout>>,
    process: CliProcess,
}

/// One entry of `myrmic telemetry debug --json`.
#[derive(Debug)]
pub enum DebugEntry {
    /// a command stored in a cell's mailbox
    Command(DebugCommand),
    /// an event stored in the events mailbox
    Event(DebugEvent),
    /// a runtime log record
    Log(DebugLog),
}

/// A log record as the debug stream prints it: the [`LogRecord`] plus its tracing target.
#[derive(Debug, serde::Deserialize)]
pub struct DebugLog {
    pub target: Option<String>,
    #[serde(flatten)]
    pub record: LogRecord,
}

impl DebugListener {
    /// Start `myrmic <args>` on the target with its stdout piped here.
    ///
    /// `sh` prints its own PID and then execs the CLI in its place, so that PID is the CLI's.
    /// On a remote target it is the only handle that reaches the CLI: stopping the local `ssh` or
    /// `docker exec` would leave it running.
    pub(super) async fn spawn(backend: &impl MyrmicBackend, args: &[&str]) -> Self {
        let mut sh_args = vec!["-c", "echo $$; exec \"$@\"", "sh", backend.binary()];
        sh_args.extend_from_slice(args);
        let mut command = backend.command("sh", &sh_args);
        command.stdin(Stdio::null()).stdout(Stdio::piped());
        let invocation = format!("{command:?}");

        let mut child = command
            .spawn()
            .unwrap_or_else(|err| panic!("failed to start {invocation}: {err}"));
        let stdout = child.stdout.take().expect("stdout was configured as piped");
        let stdout = ChildStdout::from_std(stdout)
            .expect("failed to register the CLI's stdout with the tokio runtime");
        let mut lines = BufReader::new(stdout).lines();

        let pid = lines
            .next_line()
            .await
            .unwrap_or_else(|err| panic!("failed to read the PID of {invocation}: {err}"))
            .unwrap_or_else(|| panic!("{invocation} exited before reporting its PID"));
        let pid: u32 = pid
            .parse()
            .unwrap_or_else(|err| panic!("{invocation} reported `{pid}` as its PID: {err}"));
        let interrupt = backend.command("sh", &["-c", "kill -INT \"$1\"", "sh", &pid.to_string()]);

        Self {
            lines,
            process: CliProcess { child, interrupt },
        }
    }

    /// the next entry of the debug stream, `None` once the process has closed its stdout
    pub async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        self.lines.next_line().await
    }

    /// Wait for the next entry of the debug stream and parse it.
    pub async fn next_entry(&mut self) -> DebugEntry {
        let line = self
            .next_line()
            .await
            .expect("failed to read the debug stream")
            .expect("myrmic telemetry debug exited");
        let value: serde_json::Value = serde_json::from_str(&line)
            .unwrap_or_else(|err| panic!("debug stream printed invalid JSON ({err}): {line}"));

        if let Ok(command) = DebugCommand::deserialize(&value) {
            DebugEntry::Command(command)
        } else if let Ok(event) = DebugEvent::deserialize(&value) {
            DebugEntry::Event(event)
        } else {
            // `LogRecord` defaults every missing field, so it would accept any object. This key
            // is always printed for a log record and never for a mailbox entry.
            assert!(
                value.get("observedTimeUnixNano").is_some(),
                "debug stream printed neither a mailbox entry nor a log record: {line}"
            );
            let log = DebugLog::deserialize(value)
                .unwrap_or_else(|err| panic!("malformed log record ({err}): {line}"));
            DebugEntry::Log(log)
        }
    }

    /// [`Self::next_entry`], or `None` if no entry arrives within `timeout`. A line that was
    /// only partly read when the deadline passed is kept for the next call.
    pub async fn next_entry_timeout(&mut self, timeout: Duration) -> Option<DebugEntry> {
        tokio::time::timeout(timeout, self.next_entry()).await.ok()
    }
}

// Stopping the CLI takes three steps in this order: interrupt it, close our end of its stdout
// pipe, wait for it to exit. The pipe must be closed before waiting, because a CLI blocked
// writing into a full pipe never gets to handle the interrupt. It must not be closed before the
// interrupt either: the CLI or its `ssh`/`docker exec` wrapper may then exit on the broken pipe
// first, and `interrupt` would skip a CLI that is still running on the remote side, or fail on
// one that just exited. A `Drop` impl cannot drop a single field early, so the steps are spread
// over the drop order: this impl interrupts, then `lines` drops, then `process` drops and waits.
impl Drop for DebugListener {
    fn drop(&mut self) {
        self.process.interrupt();
    }
}

/// The local end of a CLI started by [`DebugListener::spawn`]: the CLI itself for the local
/// backend, the `ssh`/`docker exec` wrapper around it otherwise. Waits for it on drop.
struct CliProcess {
    child: std::process::Child,
    /// sends `SIGINT` to the CLI on the target
    interrupt: std::process::Command,
}

impl CliProcess {
    /// Send `SIGINT` to the CLI, unless it has already exited.
    fn interrupt(&mut self) {
        match self.child.try_wait() {
            // the stream ended on its own, nothing is left to interrupt
            Ok(Some(_)) => {}
            Ok(None) => match self.interrupt.output() {
                Ok(output) if output.status.success() => {}
                Ok(output) => eprintln!(
                    "DebugListener: failed to interrupt `myrmic telemetry debug`: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
                Err(err) => eprintln!("DebugListener: failed to run {:?}: {err}", self.interrupt),
            },
            Err(err) => eprintln!("DebugListener: failed to check on the CLI: {err}"),
        }
    }
}

impl Drop for CliProcess {
    fn drop(&mut self) {
        if let Err(err) = self.child.wait() {
            eprintln!("DebugListener: failed to wait for `myrmic telemetry debug` to exit: {err}");
        }
    }
}
