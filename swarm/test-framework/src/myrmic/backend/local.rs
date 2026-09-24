use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize as _;
use swarm_telemetry::db::opentelemetry_proto::tonic::logs::v1::LogRecord;
use swarm_telemetry::debug::{DebugCommand, DebugEvent};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout};

use crate::myrmic::{BuildTarget, cell::CellSpec};

use super::{MyrmicBackend, parse_runtime_list, parse_status_lines};

const INFO_PREFIX: &str = "INFO  ";

/// [`MyrmicBackend`] that runs a myrmic binary on the host.
#[derive(Clone)]
pub struct LocalBinary {
    binary: PathBuf,
}

/// A running `myrmic telemetry debug`, read line by line. Dropping it kills the process.
pub struct DebugListener {
    // only held so the process lives as long as the listener (spawned with `kill_on_drop`)
    _process: Child,
    lines: Lines<BufReader<ChildStdout>>,
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

impl LocalBinary {
    /// wrap the myrmic binary at `binary`
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub(crate) async fn build(&self, cell_path: &std::path::Path, target: BuildTarget) {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("build");
        match target {
            BuildTarget::Wasm => {
                cmd.arg("--target").arg("linux");
            }
            BuildTarget::WasmWithApi => {}
        }
        let output = cmd.arg(cell_path).output().await.unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("myrmic build failed for {}", cell_path.display());
        }
    }

    /// Deploy a cell and return the CLI's diagnostic output.
    pub async fn deploy_with_output(&self, cell: &CellSpec, srn: &str, tags: &[&str]) -> String {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("deploy").arg("--name").arg(srn).arg(cell.as_path());
        for tag in tags.iter().copied() {
            cmd.arg("--tag").arg(tag);
        }
        let output = cmd.output().await.unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("deploy failed");
        }

        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    /// Deploy an app and return the CLI's diagnostic output.
    pub(crate) async fn deploy_app_with_output(&self, app_spec: &std::path::Path) -> String {
        let output = tokio::process::Command::new(&self.binary)
            .arg("deploy")
            .arg(app_spec)
            .output()
            .await
            .unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("deploy app failed for {}", app_spec.display());
        }

        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    /// Run `myrmic telemetry set-db-retention`, so connected nodes persist their telemetry for
    /// `retention` (humantime, e.g. `1h`).
    pub(crate) async fn set_db_retention(&self, retention: &str) {
        let output = tokio::process::Command::new(&self.binary)
            .arg("telemetry")
            .arg("set-db-retention")
            .arg(retention)
            .output()
            .await
            .unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("telemetry set-db-retention failed");
        }
    }

    /// Start `myrmic telemetry debug --json`, optionally filtered to the cell `id` (SRI or SRN)
    /// and with the cell log level raised to `level`.
    pub(crate) fn get_debug_listener(
        &self,
        id: Option<&str>,
        level: Option<&str>,
    ) -> DebugListener {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("telemetry").arg("debug").arg("--json");
        if let Some(id) = id {
            cmd.arg("--id").arg(id);
        }
        if let Some(level) = level {
            cmd.arg("--level").arg(level);
        }
        let mut process = cmd
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn myrmic telemetry debug");
        let stdout = process
            .stdout
            .take()
            .expect("stdout was configured as piped");

        DebugListener {
            _process: process,
            lines: BufReader::new(stdout).lines(),
        }
    }

    /// Drop-only best-effort helper — used by the blocking delete variants; do not use for happy-path operations.
    fn run_blocking(&self, args: &[&str]) -> Result<(), String> {
        let output = std::process::Command::new(&self.binary)
            .args(args)
            .output()
            .map_err(|e| format!("failed to run myrmic {args:?}: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "myrmic {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }
}

impl MyrmicBackend for LocalBinary {
    async fn send(&self, sri: &str, command: &str) -> Option<String> {
        let output = tokio::process::Command::new(&self.binary)
            .arg("send")
            .arg(sri)
            .arg(command)
            .output()
            .await
            .unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("myrmic send {sri}/{command} failed");
        }

        // myrmic writes all output (including command responses) to stderr via info!().
        // Format: "INFO  <message>" (label has a trailing space, format adds another).
        // The send command also emits a "trace ID = ..." line first; skip it.
        // "(no response)" means the command returned no payload.
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter_map(|line| line.strip_prefix(INFO_PREFIX))
            .rfind(|msg| !msg.starts_with("trace ID = "))
            .and_then(|msg| {
                if msg == "(no response)" {
                    None
                } else {
                    Some(msg.to_owned())
                }
            })
    }

    async fn start_runtime(&self, name: &str, tags: &[&str]) {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("runtimes")
            .arg("start")
            .arg("-d")
            .arg("--name")
            .arg(name);
        for tag in tags.iter().copied() {
            cmd.arg("--tag").arg(tag);
        }
        let output = cmd.output().await.unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("runtimes start failed");
        }
    }

    async fn delete_runtime(&self, name: &str) {
        let output = tokio::process::Command::new(&self.binary)
            .arg("runtimes")
            .arg("delete")
            .arg(name)
            .output()
            .await
            .unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("runtimes delete failed");
        }
    }

    async fn list_runtimes(&self) -> Vec<String> {
        let output = tokio::process::Command::new(&self.binary)
            .arg("runtimes")
            .arg("list")
            .output()
            .await
            .unwrap();

        if output.status.success() {
            parse_runtime_list(&String::from_utf8_lossy(&output.stdout))
        } else {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("runtimes list failed");
        }
    }

    async fn new_cell(&self, path: &std::path::Path, name: &str, sdk: Option<&str>) {
        let path = path.display().to_string();

        let mut options = vec!["new"];
        if let Some(sdk) = sdk {
            options.extend_from_slice(&["--sdk", sdk]);
        }
        options.extend_from_slice(&["--name", name, path.as_str()]);

        let output = tokio::process::Command::new(&self.binary)
            .args(options)
            .output()
            .await
            .unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("new failed");
        }
    }

    async fn deploy(&self, cell: CellSpec, srn: &str, tags: &[&str]) {
        self.deploy_with_output(&cell, srn, tags).await;
    }

    async fn deploy_app(&self, app_spec: &std::path::Path) {
        self.deploy_app_with_output(app_spec).await;
    }

    async fn delete_cell(&self, sri: &str) {
        let output = tokio::process::Command::new(&self.binary)
            .arg("delete")
            .arg("--cell")
            .arg(sri)
            .output()
            .await
            .unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("delete failed");
        }
    }

    async fn status(&self) -> Vec<String> {
        let output = tokio::process::Command::new(&self.binary)
            .arg("cells")
            .arg("status")
            .output()
            .await
            .unwrap();

        if output.status.success() {
            parse_status_lines(&String::from_utf8_lossy(&output.stdout))
        } else {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("status failed");
        }
    }

    fn delete_runtime_blocking(&self, name: &str) -> Result<(), String> {
        self.run_blocking(&["runtimes", "delete", name])
    }

    fn delete_cell_blocking(&self, sri: &str) -> Result<(), String> {
        self.run_blocking(&["delete", "--cell", sri])
    }
}
