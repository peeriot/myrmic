use crate::docker::container::ConnectedContainer;
use crate::docker::image::Image;
use crate::docker::{CommandOutput, init_docker, managed::ManagedContainer};
use crate::myrmic::cell::CellSpec;

use super::{MyrmicBackend, parse_runtime_list, parse_status_lines};

/// [`MyrmicBackend`] that runs the myrmic CLI inside a docker container.
#[derive(Clone)]
pub struct DockerBinary {
    container: ManagedContainer,
}

impl DockerBinary {
    /// attach to an existing container that has a myrmic binary on its `PATH`
    pub fn attach(container: ConnectedContainer) -> Self {
        Self {
            container: container.into(),
        }
    }

    /// run an existing image (with an idle command) and execute myrmic inside it
    pub async fn run_image(image: impl Into<String>, name: &str) -> Self {
        let docker = init_docker();
        let image = Image::new(image.into());
        let container = image.run_idle(docker, name).await;
        Self { container }
    }

    /// build an image from `dockerfile` (with the myrmic `binary` in the build context) and run it
    pub async fn build_and_run(
        dockerfile: impl AsRef<std::path::Path>,
        binary: impl AsRef<std::path::Path>,
        name: &str,
    ) -> Self {
        let docker = init_docker();
        let image_tag = "myrmic-e2e:latest";
        let binary = binary.as_ref();
        let image = Image::build(
            &docker,
            image_tag,
            dockerfile.as_ref(),
            &[(binary, binary.file_name().unwrap().to_str().unwrap())],
        )
        .await;

        let container = image.run_idle(docker, name).await;
        Self { container }
    }

    /// the container the myrmic CLI is executed in
    pub fn container(&self) -> &ConnectedContainer {
        &self.container
    }

    async fn exec(&self, args: &[&str]) -> CommandOutput {
        let mut cmd = vec!["myrmic"];
        cmd.extend_from_slice(args);
        self.container.exec(&cmd).await
    }

    /// stop the container according to its cleanup options
    pub async fn stop(&self) {
        self.container.cleanup().await;
    }

    /// Drop-only best-effort helper — used by the blocking delete variants; do not use for happy-path operations.
    fn exec_blocking(&self, myrmic_args: &[&str]) -> Result<(), String> {
        let mut args = vec!["exec", self.container.id(), "myrmic"];
        args.extend_from_slice(myrmic_args);
        let output = std::process::Command::new("docker")
            .args(&args)
            .output()
            .map_err(|e| format!("failed to run docker {args:?}: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "docker {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }
}

impl MyrmicBackend for DockerBinary {
    async fn cleanup(&self) {
        self.stop().await;
    }

    async fn send(&self, sri: &str, command: &str) -> Option<String> {
        let output = self.exec(&["send", sri, command]).await;
        if !output.success {
            eprintln!("{}", output.stderr);
            panic!("myrmic send {sri}/{command} failed");
        }
        let stdout = output.stdout.trim().to_owned();
        if stdout.is_empty() {
            None
        } else {
            Some(stdout)
        }
    }

    async fn start_runtime(&self, name: &str, tags: &[&str]) {
        let mut args = vec!["runtimes", "start", "-d", "--name", name];
        for tag in tags.iter().copied() {
            args.push("--tag");
            args.push(tag);
        }
        let output = self.exec(&args).await;
        if !output.success {
            eprintln!("{}", output.stderr);
            panic!("runtimes start failed");
        }
    }

    async fn delete_runtime(&self, name: &str) {
        let output = self.exec(&["runtimes", "delete", name]).await;
        if !output.success {
            eprintln!("{}", output.stderr);
            panic!("runtimes delete failed");
        }
    }

    async fn list_runtimes(&self) -> Vec<String> {
        let output = self.exec(&["runtimes", "list"]).await;
        if !output.success {
            eprintln!("{}", output.stderr);
            panic!("runtimes list failed");
        }
        parse_runtime_list(&output.stdout)
    }

    async fn new_cell(&self, path: &std::path::Path, name: &str, sdk: Option<&str>) {
        let path = path.display().to_string();

        let mut options = vec!["new"];
        if let Some(sdk) = sdk {
            options.extend_from_slice(&["--sdk", sdk]);
        }
        options.extend_from_slice(&["--name", name, path.as_str()]);

        let output = self.exec(&options).await;
        if !output.success {
            eprintln!("{}", output.stderr);
            panic!("new failed");
        }
    }

    async fn deploy(&self, cell: CellSpec, sri: &str, tags: &[&str]) {
        let cell_path = cell.as_path().display().to_string();
        let mut args = vec!["deploy", "--sri", sri, cell_path.as_str()];
        for tag in tags.iter().copied() {
            args.push("--tag");
            args.push(tag);
        }
        let output = self.exec(&args).await;
        if !output.success {
            eprintln!("{}", output.stderr);
            panic!("deploy failed");
        }
    }

    async fn deploy_app(&self, app_spec: &std::path::Path) {
        let app_spec_path = app_spec.display().to_string();
        let output = self.exec(&["deploy", app_spec_path.as_str()]).await;
        if !output.success {
            eprintln!("{}", output.stderr);
            panic!("deploy app failed");
        }
    }

    async fn delete_cell(&self, sri: &str) {
        let output = self.exec(&["delete", sri]).await;
        if !output.success {
            eprintln!("{}", output.stderr);
            panic!("delete failed");
        }
    }

    async fn status(&self) -> Vec<String> {
        let output = self.exec(&["cells", "status"]).await;
        if !output.success {
            eprintln!("{}", output.stderr);
            panic!("runtimes list failed");
        }
        parse_status_lines(&output.stdout)
    }

    fn delete_runtime_blocking(&self, name: &str) -> Result<(), String> {
        self.exec_blocking(&["runtimes", "delete", name])
    }

    fn delete_cell_blocking(&self, sri: &str) -> Result<(), String> {
        self.exec_blocking(&["delete", sri])
    }
}
