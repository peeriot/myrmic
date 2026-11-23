//! Cells for a swarm's runtimes, deployed through its sorg client: a cell's binaries
//! ([`Binaries`]: its wasm, built or pre-built, and AOT compilations of it for embedded runtimes)
//! are registered once as a class ([`Class`]), and any number of cells are deployed from it, each
//! under an SRI of its own ([`Swarm::deploy`], [`Swarm::deploy_all`]). The [`Swarm`] methods that
//! do so are in its own module; they read the fields here, which is why those are visible to it.
//!
//! ```ignore
//! let binaries = cell::Binaries::from_file(prebuilt_cell_wasm(DUMMY_CELL));
//! let class = swarm.register(&binaries).await?;
//! swarm.deploy(&class, Sri::from_target("a")?, &["gpu"], None).await?;
//! ```
//!
//! For one cell from a crate or wasm file, the CLI does all three steps in one call
//! ([`crate::myrmic::Myrmic::cell`]); these are for what it can't: many cells of one class,
//! several cells in one request, and AOTs the test built itself.

use std::path::{Path, PathBuf};

use cell_protocol::{ArtifactPlatform, BlobHash, Sri};

#[cfg(doc)]
use super::Swarm;

/// A cell's binaries, registered as one class: its wasm, under the class name of the wasm file's
/// name (e.g. `my_cell.wasm`), and the AOT compilations of that wasm a runtime of an embedded
/// platform runs instead (none unless added with [`Self::aot`]).
pub struct Binaries {
    pub(super) name: String,
    pub(super) wasm: PathBuf,
    pub(super) aots: Vec<(ArtifactPlatform, PathBuf)>,
}

/// A class a swarm's class registry has (see [`Swarm::register`]): cells deploy from it.
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

    /// The binaries with the AOT compilation of their wasm at `path` for `platform`, e.g. what
    /// `myrmic_build::build` produced for an embedded target alongside the wasm.
    #[must_use]
    pub fn aot(mut self, platform: ArtifactPlatform, path: impl AsRef<Path>) -> Self {
        self.aots.push((platform, path.as_ref().to_owned()));
        self
    }
}
