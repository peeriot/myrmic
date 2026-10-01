//! Registration of the prebuilt cell fixture wasm modules used in the integration tests of sorg components

use std::path::{Path, PathBuf};

use cell_protocol::{AddMode, ClassArtifact};
use swarm::spawn::Spawned;

/// Env var through which the `build-cell-fixtures` nextest setup script hands
/// the tests a `PATH`-style list of `<crate dir>=<wasm module>` entries.
const CELL_FIXTURES_ENV: &str = "SORG_TESTS_CELL_FIXTURES";

/// Registers the prebuilt wasm module of a cell fixture as a class artifact.
pub async fn register_fixture_class(
    logic_crate_path: impl Into<PathBuf>,
    cell_name: &str,
    db_swarm: &Spawned,
) {
    let wasm = prebuilt_cell_wasm(logic_crate_path);
    let class_name = format!("{cell_name}.wasm");
    register_class_artifact(wasm.to_str().unwrap(), &class_name, db_swarm).await;
}

/// Registers the prebuilt wasm module of a cell fixture as a class artifact,
/// deriving the class name from the crate directory.
pub async fn register_fixture(binary_path: impl Into<PathBuf>, db_swarm: &Spawned) {
    let path = binary_path.into();
    let class_name = format!("{}.wasm", module_name_from_path(&path));
    let wasm = prebuilt_cell_wasm(&path);
    register_class_artifact(wasm.to_str().unwrap(), &class_name, db_swarm).await;
}

/// Looks up the wasm module the `build-cell-fixtures` nextest setup script
/// (`sorg-tests/build-cell-fixtures.sh`) built for the cell crate at
/// `logic_crate_path`. Tests never run cargo themselves: every build contends
/// for the same target dir lock, which made fixture builds dominate test time.
fn prebuilt_cell_wasm(logic_crate_path: impl Into<PathBuf>) -> PathBuf {
    let crate_dir = logic_crate_path.into();
    let crate_dir = crate_dir
        .canonicalize()
        .unwrap_or_else(|err| panic!("no cell crate at {}: {err}", crate_dir.display()));
    let fixtures = std::env::var(CELL_FIXTURES_ENV).unwrap_or_else(|_| {
        panic!(
            "{CELL_FIXTURES_ENV} is not set; run the tests with `cargo nextest run`, whose \
             setup script builds the cell fixtures"
        )
    });

    let Some((_, wasm)) = fixtures
        .split(':')
        .map(|entry| {
            entry
                .split_once('=')
                .expect("cell fixture entries are `<crate dir>=<wasm module>`")
        })
        .find(|(dir, _)| Path::new(dir) == crate_dir)
    else {
        panic!(
            "{} is not in {CELL_FIXTURES_ENV}; only crates in `tests/fixtures` are prebuilt",
            crate_dir.display()
        )
    };
    PathBuf::from(wasm)
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
