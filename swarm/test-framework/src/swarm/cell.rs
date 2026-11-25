//! Cells deployed through a swarm's sorg client. A cell's binaries ([`Binaries`]) are its wasm,
//! built from a crate or read from a file, and optionally AOT compilations of that wasm for
//! embedded runtimes. A test registers the binaries once as a class ([`Class`]) and then deploys
//! any number of cells from it, each under an SRI of its own ([`Swarm::deploy`],
//! [`Swarm::deploy_all`]). Those [`Swarm`] methods live in the swarm's module and read the fields
//! of the types here, which is why the fields are visible to it.
//!
//! ```ignore
//! let binaries = cell::Binaries::build("cells/counter").await;
//! let class = swarm.register(&binaries).await?;
//! for i in 0..100 {
//!     let sri = cell_protocol::Sri::of_path(&format!("counter/{i}"))?;
//!     swarm.deploy(&class, sri, &["gpu"], None).await?;
//! }
//! ```
//!
//! To deploy one cell from a crate or a wasm file, the CLI builds, registers and deploys it in
//! one call ([`crate::myrmic::Myrmic::cell`]). The types here are for what the CLI can't do: many
//! cells of one class, several cells in one request, and AOT compilations the test built itself.

use std::path::{Path, PathBuf};

use cell_protocol::{ArtifactPlatform, BlobHash, Sri};

#[cfg(doc)]
use super::Swarm;

/// A cell's binaries, registered as one class: its wasm, and the AOT compilations of that wasm that
/// runtimes of embedded platforms run instead (none until added with [`Self::aot`]). The class is
/// named after the wasm file (e.g. `my_cell.wasm`).
pub struct Binaries {
    pub(super) name: String,
    pub(super) wasm: PathBuf,
    pub(super) aots: Vec<(ArtifactPlatform, PathBuf)>,
}

/// A class in a swarm's class registry (see [`Swarm::register`]). Cells are deployed from it.
#[derive(Clone, Copy)]
pub struct Class {
    pub(super) hash: BlobHash,
}

/// One cell of a request that deploys several at once (see [`Swarm::deploy_all`]).
pub struct Deployment<'a> {
    pub class: &'a Class,
    pub sri: Sri,
    /// the tags a runtime must have to run the cell; none for any runtime
    pub tags: &'a [&'a str],
}

impl Binaries {
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
        let wasm = path.as_ref().to_owned();
        let name = wasm
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("{} has no UTF-8 file name", wasm.display()))
            .to_owned();
        Self {
            name,
            wasm,
            aots: vec![],
        }
    }

    /// Add the AOT compilation of the wasm for `platform`, at `path`, e.g. what
    /// `myrmic_build::build` produced for an embedded target next to the wasm.
    #[must_use]
    pub fn aot(mut self, platform: ArtifactPlatform, path: impl AsRef<Path>) -> Self {
        self.aots.push((platform, path.as_ref().to_owned()));
        self
    }
}
