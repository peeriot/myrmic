use std::path::PathBuf;
use std::process::Command;
use textus::Template as _;

#[derive(textus::Template)]
#[template(path = "tests/templates/workspace-app")]
struct WorkspaceAppTemplate {}

#[test]
#[ignore = "needs the revision the CLI was built from pushed to the swarm remote, \
            plus credentials to fetch it"]
fn default_git_sdk_project_builds() {
    let project = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .arg("new")
        .arg(project.path())
        .output()
        .expect("failed to run myrmic new");

    assert!(
        output.status.success(),
        "myrmic new failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let manifest = project.path().join("Cargo.toml");

    assert!(
        manifest.exists(),
        "generated project should contain Cargo.toml"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .arg("build")
        .arg(&manifest)
        .arg("--platform")
        .arg("linux")
        .env_remove("RUSTFLAGS")
        .output()
        .expect("failed to run myrmic build for generated project");

    assert!(
        output.status.success(),
        "generated project failed to build\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let _ = project.close();
}

#[test]
fn workspace_member_app_build_finds_workspace_target_wasm() {
    let workspace = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let cell = workspace.path().join("my-cell");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .arg("new")
        .arg(&cell)
        .arg("--sdk")
        .arg(local_sdk())
        .output()
        .expect("failed to run myrmic new");

    assert!(
        output.status.success(),
        "myrmic new failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    WorkspaceAppTemplate {}
        .render_into(workspace.path())
        .expect("failed to write workspace fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .current_dir(&workspace)
        .arg("build")
        .arg("app.yml")
        .env_remove("RUSTFLAGS")
        .output()
        .expect("failed to run myrmic build for workspace app");

    assert!(
        output.status.success(),
        "workspace app failed to build\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(
        workspace
            .path()
            .join("target/wasm32-unknown-unknown/release/my_cell.wasm")
            .exists(),
        "cargo should write the wasm artifact under the workspace target directory"
    );
    assert!(
        !cell.join("target").exists(),
        "the fixture should not have a member-local target directory"
    );
    assert!(
        workspace.path().join("my-app.nest").exists(),
        "myrmic build should write a nest archive named after the app"
    );
}

/// With no `--sdk`, the scaffolded crate pins `myrmic-sdk` to the swarm repo at
/// the revision this CLI was built from — not a stale hardcoded revision.
#[test]
fn new_pins_sdk_to_the_build_revision() {
    let project = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let cell = project.path().join("my-cell");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .arg("new")
        .arg(&cell)
        .output()
        .expect("failed to run myrmic new");

    assert!(
        output.status.success(),
        "myrmic new failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let manifest = std::fs::read_to_string(cell.join("Cargo.toml"))
        .expect("generated project should contain Cargo.toml");

    assert!(
        manifest.contains("ssh://git@github.com/peeriot/swarm.git"),
        "generated manifest should depend on the swarm git repo, got:\n{manifest}"
    );
    assert!(
        manifest.contains("rev = \""),
        "generated manifest should pin a revision, got:\n{manifest}"
    );
    assert!(
        !manifest.contains("d0014374"),
        "generated manifest should not use the old hardcoded revision, got:\n{manifest}"
    );

    let _ = project.close();
}

/// The scaffold ships a `no_std`-compatible `serde` with `derive`: `State<T>`
/// bounds `T` on `Serialize + DeserializeOwned`, so the first custom type a
/// user puts in state needs it and nothing else pulls it in.
#[test]
fn new_scaffolds_serde_for_state_types() {
    let project = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let cell = project.path().join("my-cell");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .arg("new")
        .arg(&cell)
        .arg("--sdk")
        .arg(local_sdk())
        .output()
        .expect("failed to run myrmic new");

    assert!(
        output.status.success(),
        "myrmic new failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let manifest = std::fs::read_to_string(cell.join("Cargo.toml"))
        .expect("generated project should contain Cargo.toml");

    let serde = manifest
        .lines()
        .find(|line| line.starts_with("serde ="))
        .unwrap_or_else(|| panic!("generated manifest should depend on serde, got:\n{manifest}"));

    assert!(
        serde.contains("default-features = false"),
        "serde must stay off `std` for a no_std cell, got:\n{serde}"
    );
    for feature in ["alloc", "derive"] {
        assert!(
            serde.contains(feature),
            "serde should enable the `{feature}` feature, got:\n{serde}"
        );
    }

    let _ = project.close();
}

fn local_sdk() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("sdk/myrmic-sdk")
}

/// `--sdk <version>` scaffolds a registry dependency in cargo's canonical short
/// form — what a release CLI bakes in as its default via `MYRMIC_SDK_VERSION`.
#[test]
fn new_with_a_version_sdk_renders_a_registry_dep() {
    let project = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let cell = project.path().join("my-cell");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .arg("new")
        .arg(&cell)
        .arg("--sdk")
        .arg("0.2.1")
        .output()
        .expect("failed to run myrmic new");

    assert!(
        output.status.success(),
        "myrmic new failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let manifest = std::fs::read_to_string(cell.join("Cargo.toml"))
        .expect("generated project should contain Cargo.toml");

    assert!(
        manifest
            .lines()
            .any(|line| line == r#"myrmic-sdk = "0.2.1""#),
        "generated manifest should depend on the published sdk, got:\n{manifest}"
    );

    let _ = project.close();
}
