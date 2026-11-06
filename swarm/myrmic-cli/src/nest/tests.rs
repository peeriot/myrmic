use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sorg_common::{RestartPolicy, RestartType};

use crate::archive;
use crate::args::Ctx;
use crate::build::{AppInfo, CellClass};
use crate::models::CellInstance;

const WASM: &[u8] = b"wasm bytes shared by both classes";
const AOT: &[u8] = b"riscv32imac aot bytes";

fn blob(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write blob");
    path
}

/// Two classes built from byte-identical wasm, one with an AOT artifact, and
/// instances that use every per-instance setting.
fn app(dir: &Path) -> AppInfo {
    AppInfo {
        name: "demo".to_owned(),
        instances: vec![
            CellInstance {
                id: "alpha".to_owned(),
                srn: Some("alpha-1".to_owned()),
                tags: vec!["edge".to_owned(), "gpu".to_owned()],
                arguments: Some(br#"{"greeting":"hi"}"#.to_vec()),
                restart: Some(RestartPolicy::with_default_bounds(RestartType::Always)),
            },
            CellInstance {
                id: "alpha".to_owned(),
                srn: Some("alpha-2".to_owned()),
                tags: vec![],
                arguments: None,
                restart: Some(RestartPolicy {
                    restart_type: RestartType::OnError,
                    max_restarts: 2,
                    window_ms: 30_000,
                    delay_ms: 250,
                }),
            },
            CellInstance {
                id: "beta".to_owned(),
                srn: Some("beta".to_owned()),
                tags: vec!["lab".to_owned()],
                arguments: Some(vec![0xde, 0xad, 0xbe, 0xef]),
                restart: None,
            },
        ],
        classes: HashMap::from([
            (
                "alpha".to_owned(),
                CellClass {
                    name: "alpha".to_owned(),
                    wasm_path: Some(blob(dir, "alpha.wasm", WASM)),
                    riscv32imac: Some(blob(dir, "alpha.aot", AOT)),
                },
            ),
            (
                "beta".to_owned(),
                CellClass {
                    name: "beta".to_owned(),
                    wasm_path: Some(blob(dir, "beta.wasm", WASM)),
                    riscv32imac: None,
                },
            ),
        ]),
        mqtt_bridges: vec![],
        http_bridges: vec![],
    }
}

fn round_trip(dir: &Path, info: AppInfo) -> AppInfo {
    let path = dir.join("demo.nest");
    super::write(&Ctx::default(), &path, info).expect("write nest");
    super::read(&Ctx::default(), &path).expect("read nest")
}

fn read_blob(path: Option<&PathBuf>) -> Option<Vec<u8>> {
    path.map(|path| std::fs::read(path).expect("read blob"))
}

#[test]
fn round_trip_keeps_instance_settings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let back = round_trip(dir.path(), app(dir.path()));
    assert_eq!(back.instances, app(dir.path()).instances);
}

#[test]
fn round_trip_keeps_class_artifacts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let back = round_trip(dir.path(), app(dir.path()));
    let alpha = &back.classes["alpha"];
    assert_eq!(read_blob(alpha.wasm_path.as_ref()).as_deref(), Some(WASM));
    assert_eq!(read_blob(alpha.riscv32imac.as_ref()).as_deref(), Some(AOT));
    let beta = &back.classes["beta"];
    assert_eq!(read_blob(beta.wasm_path.as_ref()).as_deref(), Some(WASM));
    assert_eq!(beta.riscv32imac, None);
}

#[test]
fn identical_wasm_keeps_instances_on_their_own_class() {
    let dir = tempfile::tempdir().expect("tempdir");
    let back = round_trip(dir.path(), app(dir.path()));
    let mut placed: Vec<_> = back
        .instances
        .iter()
        .map(|instance| (instance.srn.clone().unwrap(), instance.id.clone()))
        .collect();
    placed.sort();
    assert_eq!(
        placed,
        [
            ("alpha-1".to_owned(), "alpha".to_owned()),
            ("alpha-2".to_owned(), "alpha".to_owned()),
            ("beta".to_owned(), "beta".to_owned()),
        ]
    );
}

#[test]
fn legacy_nest_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("old.nest");
    archive::write(
        &path,
        &[
            archive::Entry::virt("./app-name", "old"),
            archive::Entry::virt("./class-map.json", r#"{"alpha":"0abc"}"#),
            archive::Entry::virt("./instance-map.json", r#"{"alpha":"0abc"}"#),
        ],
    )
    .expect("write archive");
    let err = super::read(&Ctx::default(), &path)
        .err()
        .expect("legacy nest");
    assert!(format!("{err:#}").contains("rebuild"), "{err:#}");
}

#[test]
fn instance_of_unknown_class_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("broken.nest");
    archive::write(
        &path,
        &[
            archive::Entry::virt("./app-name", "broken"),
            archive::Entry::virt(
                "./classes.json",
                r#"{"alpha":{"wasm":"0abc","riscv32imac":null}}"#,
            ),
            archive::Entry::virt("./instances.json", r#"[{"id":"ghost","srn":"ghost"}]"#),
        ],
    )
    .expect("write archive");
    let err = super::read(&Ctx::default(), &path)
        .err()
        .expect("unknown class");
    assert!(format!("{err:#}").contains("ghost"), "{err:#}");
}
