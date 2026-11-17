//! Cells for a swarm's runtimes, deployed through its sorg client: a cell's wasm ([`Wasm`]), built
//! or pre-built, is registered once as a class ([`Class`]), and any number of cells are deployed
//! from it, each under an SRI of its own ([`Swarm::deploy`], [`Swarm::deploy_all`]).
//!
//! ```ignore
//! let wasm = cell::Wasm::from_file(prebuilt_cell_wasm(DUMMY_CELL));
//! let class = swarm.register(&wasm).await?;
//! swarm.deploy(&class, Sri::from_target("a")?, &["gpu"]).await?;
//! ```
//!
//! For one cell from a crate or wasm file, the CLI does all three steps in one call
//! ([`crate::myrmic::Myrmic::cell`]); these are for what it can't: many cells of one class, and
//! several cells in one request.

use std::path::{Path, PathBuf};

use cell_protocol::{BlobHash, Sri};
use sorg_common::{
    CellConfig, CellDeployment, DeployRequest, DeployResponse, DeploymentError, RequirementTags,
};

use super::Swarm;

/// A cell's wasm: the file, and the class name it is registered under, the file's name (e.g.
/// `my_cell.wasm`).
pub struct Wasm {
    name: String,
    path: PathBuf,
}

/// A class a swarm's class registry has (see [`Swarm::register`]): cells deploy from it.
#[derive(Clone, Copy)]
pub struct Class {
    hash: BlobHash,
}

/// One cell of a request that deploys several at once (see [`Swarm::deploy_all`]).
pub struct Deployment<'a> {
    pub class: &'a Class,
    pub sri: Sri,
    /// the tags a runtime must have to run the cell; none for any runtime
    pub tags: &'a [&'a str],
}

impl Wasm {
    /// Build the cell crate in `crate_dir` to wasm for Linux, as `myrmic build` does.
    pub async fn build(crate_dir: impl AsRef<Path>) -> Self {
        let manifest = crate_dir.as_ref().join("Cargo.toml");
        // cargo runs synchronously; keep it off the async runtime's worker threads
        let built = tokio::task::spawn_blocking(move || {
            myrmic_build::build(
                &manifest,
                myrmic_build::Platform::Linux,
                &myrmic_build::CargoTarget::Auto,
            )
            .unwrap_or_else(|err| panic!("failed to build {}: {err:#}", manifest.display()))
        })
        .await
        .unwrap_or_else(|err| std::panic::resume_unwind(err.into_panic()));
        Self::from_file(built.wasm)
    }

    /// The wasm file at `path`, e.g. a pre-built fixture.
    pub fn from_file(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_owned();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("{} has no UTF-8 file name", path.display()))
            .to_owned();
        Self { name, path }
    }
}

impl Swarm {
    /// Register `wasm` as a class in the swarm's class registry, for cells to deploy from.
    pub async fn register(&self, wasm: &Wasm) -> Result<Class, sorg_client::Error> {
        let bytes = tokio::fs::read(&wasm.path)
            .await
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", wasm.path.display()));
        let hash = self.sorg.add_class(&wasm.name, bytes, vec![]).await?;
        Ok(Class { hash })
    }

    /// Deploy a cell of `class` under `sri`, on a runtime that has `tags` (any runtime for none),
    /// on its own: the standalone deploy, which places one cell at a time.
    pub async fn deploy(
        &self,
        class: &Class,
        sri: Sri,
        tags: &[&str],
    ) -> Result<(), DeploymentError> {
        self.sorg
            .deploy_wasm_cell(sri, class.hash, RequirementTags::new(tags.to_vec()))
            .await
    }

    /// Deploy the cells of `deployments` in one request: the planner places them together.
    pub async fn deploy_all(
        &self,
        deployments: &[Deployment<'_>],
    ) -> Result<DeployResponse, DeploymentError> {
        let cells = deployments
            .iter()
            .map(|deployment| {
                CellDeployment::new(
                    deployment.sri,
                    CellConfig::Wasm {
                        class: deployment.class.hash,
                    },
                )
                .with_tags(RequirementTags::new(deployment.tags.to_vec()))
            })
            .collect();
        self.sorg.deploy_cells(DeployRequest::new(cells)).await
    }
}
