use std::path::PathBuf;

use crate::myrmic::{BuildTarget, cell::CellSpec};

use super::{MyrmicBackend, parse_runtime_list, parse_status_lines};

const INFO_PREFIX: &str = "INFO  ";

/// [`MyrmicBackend`] that runs a myrmic binary on the host.
#[derive(Clone)]
pub struct LocalBinary {
    binary: PathBuf,
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

    async fn deploy(&self, cell: CellSpec, sri: &str, tags: &[&str]) {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("deploy").arg("--sri").arg(sri).arg(cell.as_path());
        for tag in tags.iter().copied() {
            cmd.arg("--tag").arg(tag);
        }
        let output = cmd.output().await.unwrap();

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            panic!("deploy failed");
        }
    }

    async fn deploy_app(&self, app_spec: &std::path::Path) {
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
    }

    async fn delete_cell(&self, sri: &str) {
        let output = tokio::process::Command::new(&self.binary)
            .arg("delete")
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
        self.run_blocking(&["delete", sri])
    }
}
