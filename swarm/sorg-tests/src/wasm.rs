//! Functionality for building the wasm modules used in the integration tests of sorg components

use std::path::{Path, PathBuf};

use cell_protocol::{AddMode, ClassArtifact};
use swarm::spawn::Spawned;

/// Builds a cell via the shared `myrmic-build` pipeline and registers it as a class artifact.
pub async fn build_and_register_cell_class(
    logic_crate_path: impl Into<PathBuf>,
    cell_name: &str,
    db_swarm: &Spawned,
) {
    let wasm = compile_cell_to_wasm(logic_crate_path);
    let class_name = format!("{cell_name}.wasm");
    register_class_artifact(wasm.to_str().unwrap(), &class_name, db_swarm).await;
}

/// Builds a cell via the shared `myrmic-build` pipeline and registers it as a
/// class artifact, deriving the class name from the crate directory.
pub async fn build_cell(binary_path: impl Into<PathBuf>, db_swarm: &Spawned) {
    let path = binary_path.into();
    let class_name = format!("{}.wasm", module_name_from_path(&path));
    let wasm = compile_cell_to_wasm(&path);
    register_class_artifact(wasm.to_str().unwrap(), &class_name, db_swarm).await;
}

/// Compiles a cell logic crate to a wasm module via the shared `myrmic-build`
/// pipeline and returns the artifact path.
fn compile_cell_to_wasm(logic_crate_path: impl Into<PathBuf>) -> PathBuf {
    myrmic_build::build(
        &logic_crate_path.into().join("Cargo.toml"),
        myrmic_tags::Platform::Linux,
        &myrmic_build::CargoTarget::Auto,
    )
    .expect("myrmic build failed")
    .wasm
}

async fn register_class_artifact(wasm_path: &str, class_name: &str, db_swarm: &Spawned) {
    let bytes = std::fs::read(wasm_path).expect("failed to read wasm file");
    let artifact = ClassArtifact::Wasm(bytes);
    sorg_common::class_registry::add_class_artifact(
        db_swarm.session(),
        class_name,
        artifact,
        AddMode::Force,
    )
    .await
    .expect("failed to register class artifact");
}

fn module_name_from_path(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .file_name()
        .map(|os| os.to_string_lossy().into_owned())
        .expect("path must have a final component")
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, str::FromStr};

    use crate::wasm::module_name_from_path;

    #[test]
    fn figure_out_module_name_from_path() {
        let path_buf = PathBuf::from_str("../../wasm/modules/counter_increment").unwrap();
        let module_name = module_name_from_path(path_buf);
        assert_eq!("counter_increment", module_name);
    }
}
