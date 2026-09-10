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
        manifest.contains("https://github.com/peeriot/myrmic.git"),
        "generated manifest should depend on the myrmic git repo, got:\n{manifest}"
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

/// `--firmware` names a chip the firmware build knows. Anything else is refused
/// up front instead of scaffolding a crate whose chip feature can never resolve.
#[test]
fn new_firmware_refuses_an_unknown_chip() {
    let project = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let firmware = project.path().join("fw");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .arg("new")
        .arg(&firmware)
        .arg("--firmware=esp32")
        .arg("--sdk")
        .arg(local_sdk())
        .output()
        .expect("failed to run myrmic new");

    assert!(
        !output.status.success(),
        "myrmic new accepted an unknown chip\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("esp32c6"),
        "the error should list the supported chips, got:\n{stderr}"
    );
    assert!(
        !firmware.exists(),
        "nothing should be scaffolded for an unknown chip"
    );
}

/// `-f esp32c5` (space-separated) must be treated the same as `-f=esp32c5`:
/// the value attaches to the flag as the chip, rather than being consumed as
/// the `<PATH>` positional with the firmware silently defaulting to esp32c6.
#[test]
fn new_firmware_accepts_a_space_separated_chip() {
    let project = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let fw = project.path().join("fw");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .arg("new")
        .arg("--firmware")
        .arg("esp32c5")
        .arg(&fw)
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("esp32c5"),
        "the firmware chip should be esp32c5, not the default:\n{stderr}"
    );
    assert!(
        fw.join("partitions.toml").exists(),
        "a firmware crate should be scaffolded at the <PATH>, got only:\n{stderr}"
    );
}

fn local_sdk() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("sdk/myrmic-sdk")
}

/// The repository root, used as `--sdk` for a firmware pipeline so the
/// scaffolder resolves driver/step crates under `signal-modules/`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn new_firmware_pipeline_scaffolds_board_and_pipeline() {
    let project = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let fw = project.path().join("demo");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .args(["new", "--firmware=esp32c6", "--pipeline"])
        .arg(&fw)
        .arg("--sdk")
        .arg(repo_root())
        .output()
        .expect("failed to run myrmic new");
    assert!(
        output.status.success(),
        "myrmic new failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let board = std::fs::read_to_string(fw.join("board.yml")).expect("board.yml is scaffolded");
    assert!(
        board.contains("chip: esp32c6"),
        "board.yml keeps the chip:\n{board}"
    );
    assert!(
        board.contains("driver: sim-source"),
        "board.yml has the sim device:\n{board}"
    );
    assert!(
        board.contains("general_purpose: ["),
        "board.yml lists usable pins:\n{board}"
    );

    let pipeline = std::fs::read_to_string(fw.join("pipeline.yml")).expect("pipeline.yml");
    assert!(
        pipeline.contains("device: sim"),
        "pipeline uses the sim source:\n{pipeline}"
    );
    assert!(
        pipeline.contains("kind: retained"),
        "pipeline exposes a tap:\n{pipeline}"
    );

    let manifest = std::fs::read_to_string(fw.join("Cargo.toml")).expect("Cargo.toml");
    assert!(
        manifest.contains("pipeline = ["),
        "a pipeline feature is present:\n{manifest}"
    );
    assert!(
        manifest.contains("sim-source-driver ="),
        "the sim-source driver is seeded:\n{manifest}"
    );
    assert!(
        manifest.contains("\"dep:sim-source-driver\""),
        "the pipeline feature enables the driver dep:\n{manifest}"
    );
    assert!(
        !manifest.lines().any(|line| line.starts_with("esp-hal =")),
        "a pipeline firmware needs no direct esp-hal dependency:\n{manifest}"
    );

    let main = std::fs::read_to_string(fw.join("src/main.rs")).expect("main.rs");
    assert!(
        main.contains("esp_firmware::pipeline!()"),
        "main pulls in the pipeline:\n{main}"
    );
    assert!(
        main.contains("pipeline_pins!"),
        "main claims the pipeline pins:\n{main}"
    );

    let _ = project.close();
}

#[test]
fn new_into_existing_dir_preserves_files_on_render_failure() {
    let project = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let dir = project.path().join("existing");
    std::fs::create_dir_all(&dir).expect("create existing dir");
    std::fs::write(dir.join("keep.txt"), "precious user data").expect("write user file");
    // Block the template's `src/` directory with a file, so rendering fails.
    std::fs::write(dir.join("src"), "blocker").expect("write blocker");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .args(["new", "--pipeline"])
        .arg(&dir)
        .arg("--sdk")
        .arg(repo_root())
        .output()
        .expect("failed to run myrmic new");

    assert!(
        !output.status.success(),
        "render should fail when src/ is blocked"
    );
    assert!(
        dir.join("keep.txt").exists(),
        "a pre-existing user file must survive a failed scaffold into its directory"
    );

    let _ = project.close();
}

#[test]
fn new_pipeline_scaffolds_a_linux_project() {
    let project = tempfile::TempDir::with_prefix("myrmic-").expect("can always create a tempdir");
    let dir = project.path().join("demo");

    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .args(["new", "--pipeline"])
        .arg(&dir)
        .arg("--sdk")
        .arg(repo_root())
        .output()
        .expect("failed to run myrmic new");
    assert!(
        output.status.success(),
        "myrmic new failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let manifest = std::fs::read_to_string(dir.join("board.yml")).expect("board.yml");
    assert!(
        manifest.contains("chip: linux"),
        "manifest targets linux:\n{manifest}"
    );
    assert!(
        manifest.contains("dev_path: /dev/i2c-1"),
        "manifest names the i2c dev path:\n{manifest}"
    );
    assert!(
        manifest.contains("driver: sim-source"),
        "manifest has the sim device:\n{manifest}"
    );

    let pipeline = std::fs::read_to_string(dir.join("pipeline.yml")).expect("pipeline.yml");
    assert!(
        pipeline.contains("device: sim"),
        "pipeline uses the sim source:\n{pipeline}"
    );

    let manifest_toml = std::fs::read_to_string(dir.join("Cargo.toml")).expect("Cargo.toml");
    for dep in [
        "signal-layer-linux-rt",
        "tokio",
        "embassy-time",
        "sim-source-driver",
        "linux-codegen",
    ] {
        assert!(
            manifest_toml.contains(dep),
            "Cargo.toml is missing `{dep}`:\n{manifest_toml}"
        );
    }

    let main = std::fs::read_to_string(dir.join("src/main.rs")).expect("main.rs");
    assert!(
        main.contains("#[tokio::main]"),
        "main is a tokio binary:\n{main}"
    );
    assert!(
        main.contains("mod pipeline_config"),
        "main includes the generated module:\n{main}"
    );
    assert!(
        main.contains("setup_tap_registry"),
        "main starts the tap registry:\n{main}"
    );

    let _ = project.close();
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
