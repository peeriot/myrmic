//! Docker compose project lifecycle management for e2e tests.

use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::docker::{container::ConnectedContainer, init_docker};

/// helper to spin up a docker compose project
///
/// Dropping the project runs `docker compose down -v` best-effort (panic-safe cleanup).
/// Call [`ComposeProject::down`] instead when the test asserts on the teardown result.
pub struct ComposeProject {
    compose_file: PathBuf,
    project_name: String,
    armed: bool,
}

impl ComposeProject {
    /// a [`ComposeProject`] is initialized by running `docker compose up`
    pub async fn up(compose_file: impl Into<PathBuf>, project_name: &str) -> Self {
        let project = Self {
            compose_file: compose_file.into(),
            project_name: project_name.into(),
            armed: true,
        };

        // tear down any leftover containers from a previous run
        let _ = project.compose(&["down", "-v"]).await;

        let output = project.compose(&["up", "-d"]).await;
        assert_compose_success(&output, format_args!("docker compose up"));

        project
    }

    /// stop the project and disarm the drop guard
    pub async fn down(mut self) {
        self.armed = false;
        let output = self.compose(&["down", "-v"]).await;
        assert_compose_success(&output, format_args!("docker compose down"));
    }

    /// the compose file this project was created from
    pub fn compose_file(&self) -> &Path {
        &self.compose_file
    }

    /// the compose project name (used as `COMPOSE_PROJECT_NAME`)
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    /// the docker network name compose generates for `network` (`<project>_<network>`)
    pub fn network_name(&self, network: &str) -> String {
        format!("{}_{}", self.project_name, network)
    }

    /// find a container named `service` that was deployed with this [`ComposeProject`]
    pub async fn service_container(&self, service: &str) -> ConnectedContainer {
        let containers = self.service_containers(service).await;
        match containers.as_slice() {
            [container] => container.clone(),
            [] => panic!("no compose container found for service `{service}`"),
            _ => panic!("multiple compose containers found for service `{service}`"),
        }
    }

    /// list all service containers deployed with this [`ComposeProject`]
    pub async fn service_containers(&self, service: &str) -> Vec<ConnectedContainer> {
        let output = self.compose(&["ps", "-q", service]).await;
        assert_compose_success(
            &output,
            format_args!("docker compose ps for service `{service}`"),
        );

        let docker = init_docker();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|id| ConnectedContainer::attach(docker.clone(), id.to_owned()))
            .collect()
    }

    /// a [`crate::sidecar::Sidecar`] client for a sidecar this project exposes on localhost
    pub fn sidecar(&self, port: u16) -> crate::sidecar::Sidecar<'static> {
        // deliberate small leak: Sidecar borrows its URL and tests are short-lived processes
        let url: &'static str = Box::leak(format!("http://127.0.0.1:{port}").into_boxed_str());
        crate::sidecar::Sidecar::new(url)
    }

    async fn compose(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new("docker");
        command
            .env("COMPOSE_PROJECT_NAME", &self.project_name)
            .arg("compose")
            .arg("-f")
            .arg(&self.compose_file)
            .args(args);
        command.output().await.unwrap()
    }
}

impl Drop for ComposeProject {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let result = std::process::Command::new("docker")
            .env("COMPOSE_PROJECT_NAME", &self.project_name)
            .arg("compose")
            .arg("-f")
            .arg(&self.compose_file)
            .args(["down", "-v"])
            .output();
        match result {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "ComposeProject drop-guard: docker compose down failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(e) => eprintln!("ComposeProject drop-guard: failed to run docker: {e}"),
        }
    }
}

fn assert_compose_success(output: &std::process::Output, action: std::fmt::Arguments<'_>) {
    assert!(
        output.status.success(),
        "{action} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
