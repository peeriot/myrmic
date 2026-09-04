//! Shim around the myrmic CLI (build, deploy, runtimes) running locally or inside a container.

use std::future::Future;
use std::path::PathBuf;

pub use backend::MyrmicBackend;
use backend::docker::DockerBinary;
pub use backend::local::LocalBinary;
pub use backend::ssh::SshBinary;
use futures::FutureExt as _;

use crate::{
    clients::sorg::SorgHandle,
    docker::{container::ConnectedContainer, init_docker},
    myrmic::cell::{CellSpec, DeployedCell},
};

pub mod backend;
pub mod cell;

/// Controls which artifacts `myrmic build` produces.
#[derive(Default, Clone, Copy)]
pub enum BuildTarget {
    /// Build the wasm artifact only (`--target linux`).
    #[default]
    Wasm,
    /// Build wasm and generate the cell API file (`--target linux,api`).
    WasmWithApi,
}

/// a shim around the myrmic CLI command, it can run commands on any [`MyrmicBackend`]. Which for
/// now is either a local binary or a binary inside a container
pub struct Myrmic<B> {
    backend: B,
}

impl Myrmic<LocalBinary> {
    /// create a shim around a locally built myrmic binary (see [`crate::resolve_binary!`])
    pub fn local() -> Self {
        Self {
            backend: LocalBinary::new(crate::resolve_binary!("myrmic")),
        }
    }

    /// Open a zenoh session connected to the same swarm mesh as the myrmic CLI
    /// (peer mode, default config, multicast scouting).
    ///
    /// Use the returned session to create [`crate::clients::db::DbHandle`],
    /// [`crate::clients::sorg::SorgHandle`], etc.
    pub async fn connect_session(&self) -> zenoh::Session {
        let mut config = zenoh::Config::default();
        config
            .set_mode(Some(zenoh::config::WhatAmI::Peer))
            .expect("setting zenoh mode cannot fail");
        zenoh::open(config)
            .await
            .expect("failed to open zenoh session")
    }

    /// Build a cell using the myrmic CLI and return a `CellArtifact` that can
    /// be registered into a swarm runtime via [`crate::cell::CellArtifact::register`].
    ///
    /// For now this only works locally, but in theory can also be executed inside a container
    pub async fn build(
        &self,
        cell_path: impl Into<PathBuf>,
        target: BuildTarget,
    ) -> crate::cell::CellArtifact {
        let path = cell_path.into();
        self.backend.build(&path, target).await;
        artifact_from_path(&path)
    }
}

impl Myrmic<DockerBinary> {
    /// create a shim by attaching to a running container that has a myrmic binary
    pub fn attach(container_id: String) -> Self {
        Self {
            backend: DockerBinary::attach(ConnectedContainer::attach(init_docker(), container_id)),
        }
    }

    /// create a shim by running a container image
    pub async fn run_image(image: impl Into<String>, name: &str) -> Self {
        Self {
            backend: DockerBinary::run_image(image, name).await,
        }
    }

    /// create a shim by building a container image from `dockerfile` (with the myrmic `binary`
    /// included in the build context) and running it
    pub async fn build_and_run(
        dockerfile: impl AsRef<std::path::Path>,
        binary: impl AsRef<std::path::Path>,
        name: &str,
    ) -> Self {
        Self {
            backend: DockerBinary::build_and_run(dockerfile.as_ref(), binary.as_ref(), name).await,
        }
    }
}

impl Myrmic<SshBinary> {
    /// create a shim that runs the myrmic CLI on `host` over SSH, resolving `myrmic` on the
    /// remote user's `PATH`
    pub fn ssh(host: impl Into<String>) -> Self {
        Self {
            backend: SshBinary::new(host),
        }
    }

    /// like [`Self::ssh`], but the remote myrmic binary lives at `binary` rather than on `PATH`
    /// (e.g. a path a benchmark harness `scp`'d it to)
    pub fn ssh_at(host: impl Into<String>, binary: impl Into<String>) -> Self {
        Self {
            backend: SshBinary::at(host, binary),
        }
    }
}

impl<B> Myrmic<B>
where
    B: MyrmicBackend + Clone,
{
    /// run: myrmic runtimes start --name `name` -d [--tag `tag`...]
    ///
    /// Returns once the runtime shows up in `myrmic runtimes list`.
    pub async fn start_runtime(&self, name: &str, tags: &[&str]) -> Runtime<B> {
        self.start_runtime_at(name, tags, None).await
    }

    /// [`Self::start_runtime`], pointed at a `myrmic runtimes start <path>` config file already
    /// present on the target — see [`MyrmicBackend::start_runtime_at`].
    pub async fn start_runtime_at(
        &self,
        name: &str,
        tags: &[&str],
        config_path: Option<&str>,
    ) -> Runtime<B> {
        self.backend.start_runtime_at(name, tags, config_path).await;
        let listed = crate::wait_until(
            crate::wait::DEFAULT_TIMEOUT,
            crate::wait::DEFAULT_POLL_INTERVAL,
            || async { self.backend.list_runtimes().await.iter().any(|r| r == name) },
        )
        .await;
        assert!(
            listed,
            "runtime `{name}` not listed within 10s of `runtimes start`"
        );
        Runtime {
            backend: self.backend.clone(),
            name: name.into(),
            tags: tags.iter().map(std::string::ToString::to_string).collect(),
            armed: true,
        }
    }

    /// run: myrmic runtimes
    pub fn list_runtimes(&self) -> impl Future<Output = Vec<String>> {
        self.backend.list_runtimes()
    }

    /// run: myrmic new
    pub async fn new_cell(&self, name: &str, sdk: Option<&str>) -> CellSpec {
        let tempdir = tempfile::TempDir::with_prefix(name).unwrap();
        self.backend.new_cell(tempdir.path(), name, sdk).await;
        CellSpec::Temporary(tempdir)
    }

    /// run: myrmic send `sri` `command`
    pub fn send(
        &self,
        sri: impl Into<String>,
        command: impl Into<String>,
    ) -> impl Future<Output = Option<String>> {
        let backend = self.backend.clone();
        let sri = sri.into();
        let command = command.into();
        async move { backend.send(&sri, &command).await }
    }

    /// run: myrmic deploy `app-spec.yml` — SRIs are defined inside the app spec
    pub async fn deploy_app(&self, app_spec: impl Into<PathBuf>) {
        let path = app_spec.into();
        self.backend.deploy_app(&path).await;
    }

    /// run: myrmic deploy --sri `sri` `wasm` [--tag `tag`...]
    ///
    /// Returns once the SRI shows up in `myrmic status`.
    pub async fn deploy(&self, cell: CellSpec, sri: &str, tags: &[&str]) -> DeployedCell<B> {
        self.backend.deploy(cell, sri, tags).await;
        let deployed = crate::wait_until(
            crate::wait::DEFAULT_TIMEOUT,
            crate::wait::DEFAULT_POLL_INTERVAL,
            || async { self.is_sri_deployed(sri).await },
        )
        .await;
        assert!(
            deployed,
            "SRI `{sri}` not in `myrmic status` within 10s of deploy"
        );
        DeployedCell::new(self.backend.clone(), sri)
    }

    /// [`Self::start_runtime`] with a generated unique name (see [`Runtime::name`])
    pub async fn start_runtime_with_random_name(&self, tags: &[&str]) -> Runtime<B> {
        self.start_runtime(&uuid::Uuid::new_v4().to_string(), tags)
            .await
    }

    /// [`Self::deploy`] with a generated unique SRI (see [`DeployedCell::sri`])
    pub async fn deploy_with_random_sri(&self, cell: CellSpec, tags: &[&str]) -> DeployedCell<B> {
        let sri = uuid::Uuid::new_v4().to_string();
        self.deploy(cell, &sri, tags).await
    }

    /// check if a specific SRI is deployed by running myrmic status
    pub fn is_sri_deployed(&self, sri: &str) -> impl Future<Output = bool> {
        self.backend
            .status()
            .map(move |lines| lines.iter().any(|line| line.contains(sri)))
    }

    /// backend-specific cleanup (e.g. stopping the container for docker backends)
    pub fn cleanup(&self) -> impl Future<Output = ()> {
        self.backend.cleanup()
    }
}

/// a runtime is the outcome of `myrmic runtimes start`
///
/// Dropping a `Runtime` deletes it best-effort (panic-safe cleanup). Call
/// [`Runtime::delete`] instead when the test asserts on the post-delete state.
pub struct Runtime<B>
where
    B: MyrmicBackend,
{
    backend: B,
    name: String,
    tags: Vec<String>,
    armed: bool,
}

impl<B> Runtime<B>
where
    B: MyrmicBackend,
{
    /// Open a [`SorgHandle`] that waits for an exec runtime matching this
    /// runtime's tags and scopes all deploys to those same tags.
    pub async fn connect(&self, session: zenoh::Session) -> SorgHandle {
        let tag_refs: Vec<&str> = self.tags.iter().map(std::string::String::as_str).collect();
        SorgHandle::connect_with_tags(session, &tag_refs).await
    }

    /// the runtime name passed to `myrmic runtimes start --name`
    pub fn name(&self) -> &str {
        &self.name
    }

    /// call `myrmic runtimes delete`; returns once the runtime is no longer listed
    pub async fn delete(mut self) {
        self.armed = false;
        self.backend.delete_runtime(&self.name).await;
        let gone = crate::wait_until(
            crate::wait::DEFAULT_TIMEOUT,
            crate::wait::DEFAULT_POLL_INTERVAL,
            || async { !self.backend.list_runtimes().await.contains(&self.name) },
        )
        .await;
        assert!(
            gone,
            "runtime `{}` still listed 10s after delete",
            self.name
        );
    }
}

impl<B> Drop for Runtime<B>
where
    B: MyrmicBackend,
{
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Err(e) = self.backend.delete_runtime_blocking(&self.name) {
            eprintln!(
                "Runtime drop-guard: failed to delete runtime `{}`: {e}",
                self.name
            );
        }
    }
}

fn artifact_from_path(path: &std::path::Path) -> crate::cell::CellArtifact {
    let dir = if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
    {
        path.parent()
            .expect("Cargo.toml path must have a parent directory")
    } else {
        path
    };

    let module_name = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .expect("cell_path must have a file name");

    let target_dir = {
        let output = std::process::Command::new("cargo")
            .args(["locate-project", "--workspace", "--message-format", "plain"])
            .current_dir(dir)
            .output()
            .expect("failed to run cargo locate-project");
        assert!(
            output.status.success(),
            "failed to locate workspace: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cargo_toml =
            String::from_utf8(output.stdout).expect("non-utf8 path from cargo locate-project");
        std::path::Path::new(cargo_toml.trim())
            .parent()
            .expect("failed to get workspace root directory")
            .join("target")
    };

    let wasm_path = target_dir
        .join("wasm32-unknown-unknown/release")
        .join(format!("{module_name}.wasm"));

    crate::cell::CellArtifact {
        name: format!("{module_name}.wasm"),
        wasm_path,
    }
}
