//! Cell build artifacts and their registration into a swarm class registry.

use std::path::PathBuf;

use cell_protocol::{ArtifactPlatform, BlobHash};
use zenoh::Session;

use crate::swarm::SwarmProcess;

/// a [`CellArtifact`] is the result of building (compiling) a cell to WASM
pub struct CellArtifact {
    /// class name the artifact is registered under (e.g. `my_cell.wasm`)
    pub name: String,
    /// path to the compiled wasm module
    pub wasm_path: PathBuf,
}

impl CellArtifact {
    /// build the cell crate in `cell_dir` for linux through `myrmic_build`, as `myrmic build`
    /// does, and name the class after the wasm file cargo produced
    pub async fn build(cell_dir: PathBuf) -> Self {
        // cargo runs synchronously; keep it off the async runtime's worker threads
        tokio::task::spawn_blocking(move || {
            let wasm_path = myrmic_build::build(
                &cell_dir.join("Cargo.toml"),
                myrmic_build::Platform::Linux,
                &myrmic_build::CargoTarget::Auto,
            )
            .unwrap_or_else(|err| {
                panic!(
                    "failed to build the cell in {}: {err:#}",
                    cell_dir.display()
                )
            })
            .wasm;
            let name = wasm_path
                .file_name()
                .expect("cargo reports the wasm artifact as a file path")
                .to_string_lossy()
                .into_owned();
            Self { name, wasm_path }
        })
        .await
        .unwrap_or_else(|err| std::panic::resume_unwind(err.into_panic()))
    }

    /// register the wasm module as a class in the class registry reachable via `session`, named
    /// [`Self::name`]; returns the class id
    pub async fn register(&self, session: &Session) -> BlobHash {
        let bytes = tokio::fs::read(&self.wasm_path)
            .await
            .unwrap_or_else(|_| panic!("failed to read wasm at {}", self.wasm_path.display()));
        sorg_common::class_registry::add_class(session, &self.name, bytes, vec![])
            .await
            .expect("failed to register class")
    }

    /// [`Self::register`] using the session of a spawned [`SwarmProcess`]
    pub async fn register_on(&self, process: &SwarmProcess) -> BlobHash {
        self.register(process.session()).await
    }
}

/// an [`AotCellArtifact`] is the result of building (compiling) a cell to an AOT binary for an
/// embedded target
pub struct AotCellArtifact {
    /// class name the artifact is registered under
    pub name: String,
    /// path to the AOT compiled binary
    pub aot_path: PathBuf,
    /// target platform the AOT binary was compiled for
    pub target: ArtifactPlatform,
}

impl AotCellArtifact {
    /// register the AOT binary under [`Self::name`] in the class registry reachable via
    /// `session`: added to the wasm class the name points to, else as a class of its own;
    /// returns the AOT's id
    pub async fn register(&self, session: &Session) -> BlobHash {
        let aot = tokio::fs::read(&self.aot_path)
            .await
            .unwrap_or_else(|_| panic!("failed to read AOT binary at {}", self.aot_path.display()));
        sorg_common::class_registry::add_aot(session, &self.name, self.target, aot)
            .await
            .expect("failed to register AOT class")
    }

    /// [`Self::register`] using the session of a spawned [`SwarmProcess`]
    pub async fn register_on(&self, process: &SwarmProcess) -> BlobHash {
        self.register(process.session()).await
    }
}
