use std::process::Command;

/// `myrmic --version` reports the crate version followed by the revision the
/// binary was built from, in `<version> (<hash>)` form.
#[test]
fn version_flag_reports_cargo_version_and_revision() {
    let output = Command::new(env!("CARGO_BIN_EXE_myrmic"))
        .arg("--version")
        .output()
        .expect("failed to run myrmic --version");

    assert!(
        output.status.success(),
        "myrmic --version failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "version output should contain the crate version {:?}, got: {stdout:?}",
        env!("CARGO_PKG_VERSION"),
    );

    let Some((rev, _)) = stdout
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
    else {
        panic!("version output should carry a `(revision)` suffix, got: {stdout:?}");
    };

    assert!(
        !rev.trim().is_empty(),
        "the `(revision)` suffix should not be empty, got: {stdout:?}"
    );
}
