//! Cell build artifacts and their registration into a swarm class registry.

use std::path::PathBuf;

use cell_protocol::{AddMode, ArtifactPlatform, ClassArtifact};
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
    /// register the wasm module as a class artifact in the class registry reachable via `session`
    pub async fn register(&self, session: &Session) {
        let bytes = tokio::fs::read(&self.wasm_path)
            .await
            .unwrap_or_else(|_| panic!("failed to read wasm at {}", self.wasm_path.display()));
        sorg_common::class_registry::add_class_artifact(
            session,
            &self.name,
            ClassArtifact::Wasm(bytes),
            AddMode::Force,
        )
        .await
        .expect("failed to register class artifact");
    }

    /// [`Self::register`] using the session of a spawned [`SwarmProcess`]
    pub async fn register_on(&self, process: &SwarmProcess) {
        self.register(process.session()).await;
    }
}

/// an [`AotCellArtifact`] is the result of building (compiling) a cell to an AOT binary for an
/// embedded target
pub struct AotCellArtifact {
    /// class name the artifact is registered under
    pub name: String,
    /// path to the AOT metadata blob
    pub meta_path: PathBuf,
    /// path to the AOT compiled binary
    pub aot_path: PathBuf,
    /// target platform the AOT binary was compiled for
    pub target: ArtifactPlatform,
}

impl AotCellArtifact {
    /// register the AOT artifact as a class artifact in the class registry reachable via `session`
    pub async fn register(&self, session: &Session) {
        let meta_blob = tokio::fs::read(&self.meta_path)
            .await
            .unwrap_or_else(|_| panic!("failed to read AOT meta at {}", self.meta_path.display()));
        let aot_blob = tokio::fs::read(&self.aot_path)
            .await
            .unwrap_or_else(|_| panic!("failed to read AOT binary at {}", self.aot_path.display()));
        sorg_common::class_registry::add_class_artifact(
            session,
            &self.name,
            ClassArtifact::Aot {
                meta_blob,
                aot_blob,
                platform: self.target,
            },
            AddMode::Force,
        )
        .await
        .expect("failed to register AOT class artifact");
    }

    /// [`Self::register`] using the session of a spawned [`SwarmProcess`]
    pub async fn register_on(&self, process: &SwarmProcess) {
        self.register(process.session()).await;
    }
}
