use std::{
    path::Path,
    process::{Child, Command, Output, Stdio},
};

use sha2::{Digest, Sha256};

use crate::host;

/// Copies the local binary `local` to every host in `to`, under its own name into the host's
/// binary directory (see [`host::Ssh::new`]), to all hosts at once, and has it on the host's disk
/// before returning, so a test that cuts a host's power doesn't lose it. A host that already has a
/// file with the same content there is skipped. Panics, naming the host, if any host fails.
pub fn upload(local: &Path, to: &[&host::Ssh]) {
    let hash = sha256_of(local);
    let name = local
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| panic!("{} has no UTF-8 file name", local.display()));
    let remote_path = |host: &host::Ssh| format!("{}/{name}", host.get_bin_dir());
    // `mkdir` first, so a missing directory doesn't fail the `scp` below
    let checks = spawn_all(to, |host| {
        host.ssh(
            &["-o", "BatchMode=yes"],
            &[
                "sh",
                "-c",
                r#"mkdir -p "$(dirname "$1")" && sha256sum "$1""#,
                "sh",
                &remote_path(host),
            ],
        )
    });
    let outdated: Vec<&host::Ssh> = checks
        .into_iter()
        .filter(|(host, output)| {
            // 255 is `ssh` itself failing; any other failure means there is no file to compare
            // (the first upload) or the `mkdir` failed, which the `scp` then reports
            assert_ne!(
                output.status.code(),
                Some(255),
                "ssh to {} failed: {}",
                host.get_destination(),
                String::from_utf8_lossy(&output.stderr)
            );
            !output.status.success() || !String::from_utf8_lossy(&output.stdout).starts_with(&hash)
        })
        .map(|(host, _)| host)
        .collect();
    for (host, output) in spawn_all(&outdated, |host| {
        let mut command = super::command_with_ssh_config("scp");
        command
            .args(["-q", "-p", "-o", "BatchMode=yes"])
            .arg(local)
            .arg(format!("{}:{}", host.get_destination(), remote_path(host)));
        command
    }) {
        assert!(
            output.status.success(),
            "uploading {} to {}:{} failed: {}",
            local.display(),
            host.get_destination(),
            remote_path(host),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // the file and, for a new one, its name in the directory
    for (host, output) in spawn_all(&outdated, |host| {
        host.ssh(
            &["-o", "BatchMode=yes"],
            &[
                "sh",
                "-c",
                r#"sync -- "$1" "$(dirname "$1")""#,
                "sh",
                &remote_path(host),
            ],
        )
    }) {
        assert!(
            output.status.success(),
            "syncing {}:{} failed: {}",
            host.get_destination(),
            remote_path(host),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Runs `command(host)` for every host at once and returns each host's output once all have
/// finished.
fn spawn_all<'a>(
    hosts: &[&'a host::Ssh],
    command: impl Fn(&host::Ssh) -> Command,
) -> Vec<(&'a host::Ssh, Output)> {
    let children: Vec<(&host::Ssh, Child)> = hosts
        .iter()
        .map(|&host| {
            let child = command(host)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|e| {
                    panic!(
                        "failed to spawn a command for {}: {e}",
                        host.get_destination()
                    )
                });
            (host, child)
        })
        .collect();
    children
        .into_iter()
        .map(|(host, child)| {
            let output = child.wait_with_output().unwrap_or_else(|e| {
                panic!(
                    "failed to wait for a command for {}: {e}",
                    host.get_destination()
                )
            });
            (host, output)
        })
        .collect()
}

/// the hex SHA-256 of the file at `path`, as `sha256sum` prints it
fn sha256_of(path: &Path) -> String {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("failed to open {}: {e}", path.display()));
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    hex::encode(hasher.finalize())
}
