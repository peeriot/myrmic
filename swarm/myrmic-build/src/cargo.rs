//! Thin wrappers around the `cargo` CLI and `Cargo.toml` parsing.

use anyhow::Context as _;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Workspace context surrounding a `Cargo.toml`.
#[derive(Debug)]
pub struct Workspace {
    /// Directory containing the workspace root `Cargo.toml`.
    pub root: PathBuf,
    /// Absolute paths to each member `Cargo.toml`.
    pub members: Vec<PathBuf>,
}

/// Resolved cargo context for a specific `Cargo.toml`.
///
/// Models the three things that always coexist in a cargo invocation:
/// the manifest we resolved to, the project-wide `target/` directory, and
/// — if applicable — the package and/or surrounding workspace.
#[derive(Debug)]
pub struct CrateInfo {
    pub manifest_path: PathBuf,
    pub target_directory: PathBuf,
    /// `None` when `manifest_path` points at a virtual workspace root (no `[package]`).
    pub package_name: Option<String>,
    /// `None` when the manifest is a standalone crate with no surrounding workspace.
    pub workspace: Option<Workspace>,
}

impl CrateInfo {
    /// Returns the workspace if the current manifest is the root.
    pub fn as_root(&self) -> Option<&Workspace> {
        let w = self.workspace.as_ref()?;
        (w.root.join("Cargo.toml") == self.manifest_path).then_some(w)
    }
}

/// Read [`CrateInfo`] for the crate at `path`.
///
/// `path` may point at a `Cargo.toml` directly or at a directory inside a cargo
/// project — `cargo locate-project` is used to resolve it to the actual manifest.
pub fn crate_info(path: &Path) -> anyhow::Result<CrateInfo> {
    let manifest_path = locate_project(path)?.canonicalize()?;
    let metadata = run_metadata(Some(&manifest_path))?;

    let target_directory = target_dir(&metadata);

    let workspace_root = metadata["workspace_root"]
        .as_str()
        .map(PathBuf::from)
        .expect("cargo-metadata output is stable");

    let members: Vec<PathBuf> = metadata["packages"]
        .as_array()
        .map(|packages| {
            packages
                .iter()
                .filter_map(|p| p["manifest_path"].as_str().map(PathBuf::from))
                .collect()
        })
        .unwrap_or_default();

    // A standalone crate reports itself as the sole workspace member with the
    // workspace root pointing at its own directory. Treat that as "no workspace".
    let standalone = members.len() == 1
        && members
            .first()
            .and_then(|m| m.parent())
            .is_some_and(|p| p == workspace_root);

    let workspace = (!standalone).then_some(Workspace {
        root: workspace_root,
        members,
    });

    let package_name = metadata["packages"]
        .as_array()
        .and_then(|packages| {
            packages.iter().find(|p| {
                p["manifest_path"]
                    .as_str()
                    .map(Path::new)
                    .and_then(|p| p.canonicalize().ok())
                    .is_some_and(|p| p == manifest_path)
            })
        })
        .and_then(|p| p["name"].as_str())
        .map(str::to_owned);

    Ok(CrateInfo {
        manifest_path,
        target_directory,
        package_name,
        workspace,
    })
}

/// A cargo build target (`lib`, `bin`, …) declared by a package.
pub struct TargetInfo {
    /// Cargo target kinds, e.g. `["lib"]` or `["bin"]`.
    pub kinds: Vec<String>,
    pub name: String,
}

/// Returns the cargo targets (lib/bin/…) declared by the package at
/// `manifest_path`. Used to resolve a [`crate::CargoTarget`] selection.
pub fn package_targets(manifest_path: &Path) -> anyhow::Result<Vec<TargetInfo>> {
    let manifest_path = manifest_path.canonicalize()?;
    let metadata = run_metadata(Some(&manifest_path))?;

    let package = metadata["packages"]
        .as_array()
        .and_then(|packages| {
            packages.iter().find(|p| {
                p["manifest_path"]
                    .as_str()
                    .map(Path::new)
                    .and_then(|p| p.canonicalize().ok())
                    .is_some_and(|p| p == manifest_path)
            })
        })
        .with_context(|| format!("no package found for {}", manifest_path.display()))?;

    let targets = package["targets"]
        .as_array()
        .map(|targets| {
            targets
                .iter()
                .filter_map(|t| {
                    let name = t["name"].as_str()?.to_owned();
                    let kinds = t["kind"]
                        .as_array()?
                        .iter()
                        .filter_map(|k| k.as_str().map(str::to_owned))
                        .collect();
                    Some(TargetInfo { kinds, name })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(targets)
}

/// Resolve `path` (a directory or a `Cargo.toml`) to the nearest manifest via
/// `cargo locate-project`.
fn locate_project(path: &Path) -> std::io::Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());

    let meta = std::fs::metadata(path)?;
    let mut cmd = Command::new(cargo);
    cmd.args(["locate-project", "--message-format", "plain"]);

    if meta.is_dir() {
        cmd.current_dir(path);
    } else {
        cmd.arg("--manifest-path").arg(path);
        // Anchor cargo's working directory to the crate so toolchain resolution is driven by the
        // crate's own `rust-toolchain.toml`, not by wherever `myrmic` happened to be invoked.
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            cmd.current_dir(dir);
        }
    }

    // Capture stdout (the manifest path, parsed below) but let stderr reach the
    // terminal: resolving the manifest goes through the rustup proxy, which may
    // auto-install a pinned-but-missing toolchain here and reports its progress
    // on stderr. Inheriting it keeps that install visible instead of swallowed.
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let out = cmd.spawn()?.wait_with_output()?;

    if !out.status.success() {
        return Err(std::io::Error::other("cargo locate-project failed"));
    }

    let stdout = std::str::from_utf8(&out.stdout)
        .map_err(std::io::Error::other)?
        .trim();
    if stdout.is_empty() {
        return Err(std::io::Error::other(format!(
            "cargo locate-project returned no manifest for {}",
            path.display()
        )));
    }
    Ok(PathBuf::from(stdout))
}

fn run_metadata(manifest_path: Option<&Path>) -> anyhow::Result<Value> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());

    let mut cmd = Command::new(cargo);
    cmd.args(["metadata", "--format-version", "1", "--no-deps"]);
    if let Some(path) = manifest_path {
        cmd.arg("--manifest-path").arg(path);
        // Anchor cargo's working directory to the crate so toolchain resolution is driven by the
        // crate's own `rust-toolchain.toml`, not by wherever `myrmic` happened to be invoked.
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            cmd.current_dir(dir);
        }
    }

    // Capture stdout (the manifest path, parsed below) but let stderr reach the
    // terminal: resolving the manifest goes through the rustup proxy, which may
    // auto-install a pinned-but-missing toolchain here and reports its progress
    // on stderr. Inheriting it keeps that install visible instead of swallowed.
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let out = cmd
        .spawn()
        .context("failed to run cargo metadata")?
        .wait_with_output()
        .context("failed to run cargo metadata")?;

    if !out.status.success() {
        anyhow::bail!("cargo metadata failed");
    }

    serde_json::from_slice(&out.stdout).context("unable to parse cargo-metadata")
}

fn target_dir(metadata: &Value) -> PathBuf {
    PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .expect("cargo-metadata output is stable"),
    )
}

/// Parses `manifest_path` and returns the value at `[package.metadata.myrmic]`
/// deserialized as `M`.
///
/// Returns `Ok(None)` when the manifest has no `[package]` (virtual workspace
/// root). When `[package.metadata.myrmic]` is absent, `M::default()` is used.
pub fn read_package_metadata<M>(manifest_path: &Path) -> anyhow::Result<Option<M>>
where
    M: Default + DeserializeOwned,
{
    let content = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("unable to read: {}", manifest_path.display()))?;
    let root: toml::Value = toml::from_str(&content)
        .with_context(|| format!("unable to parse: {}", manifest_path.display()))?;

    let Some(package) = root.get("package") else {
        return Ok(None);
    };

    let metadata = package
        .get("metadata")
        .and_then(|m| m.get("myrmic"))
        .cloned();

    let parsed = match metadata {
        Some(value) => value.try_into().with_context(|| {
            format!(
                "unable to parse [package.metadata.myrmic]: {}",
                manifest_path.display()
            )
        })?,
        None => M::default(),
    };

    Ok(Some(parsed))
}

/// Expects a `cargo build` invocation, and will add `"--message-format", "json-render-diagnostics"` to the command args,
/// so we can process them on this end.
/// It extracts the artifacts from the build process, and gives them to the closure.
/// It's up to the callee to filter the ones it wants. (ie, you'll be given a lot of rlibs, which probably aren't super important)
pub fn process_cargo_build<F>(mut cmd: Command, mut func: F) -> anyhow::Result<()>
where
    F: FnMut(&Artifact),
{
    cmd.arg("--message-format").arg("json-render-diagnostics");

    cmd.stdout(Stdio::piped());
    // stderr is inherited so cargo's rendered diagnostics and progress reach the terminal (so the user can see it).
    let mut child = cmd.spawn().context("failed to run cargo build")?;
    let stdout = child.stdout.take().expect("stdout was piped");

    for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)) {
        let line = line.context("unable to read cargo output")?;
        for artifact in artifacts_in(&line) {
            func(&artifact);
        }
    }

    let status = child.wait().context("cargo failed to exit")?;
    if !status.success() {
        anyhow::bail!("build failed");
    }

    Ok(())
}

/// Whether `dir` pins its own toolchain with a `rust-toolchain.toml` (or a
/// legacy `rust-toolchain`) file, which rustup honours for a command run there.
pub fn has_toolchain_pin(dir: &Path) -> bool {
    dir.join("rust-toolchain.toml").exists() || dir.join("rust-toolchain").exists()
}

/// Whether the `cargo` a build runs in `dir` (on `toolchain` when given, as a
/// `+toolchain` override) belongs to a nightly or dev toolchain, the only ones
/// that accept `-Z` flags.
pub fn is_nightly(dir: &Path, toolchain: Option<&str>) -> anyhow::Result<bool> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(dir);
    if let Some(toolchain) = toolchain {
        cmd.arg(format!("+{toolchain}"));
    }
    cmd.arg("-vV");
    cmd.env_remove("RUSTUP_TOOLCHAIN");
    // As in `locate_project`: rustup may auto-install a pinned toolchain here
    // and reports that on stderr, so it stays visible.
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let out = cmd
        .spawn()
        .context("failed to run cargo -vV")?
        .wait_with_output()
        .context("failed to run cargo -vV")?;
    if !out.status.success() {
        anyhow::bail!("cargo -vV failed in {}", dir.display());
    }
    let version = String::from_utf8(out.stdout).context("cargo -vV printed invalid UTF-8")?;

    Ok(release_is_nightly(&version))
}

/// Sets `flags` as the rustflags for the target `target_var` names, and clears
/// `RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS`, which cargo would apply instead.
pub fn set_target_rustflags(cmd: &mut Command, target_var: &str, flags: &str) {
    cmd.env(target_var, flags);
    cmd.env_remove("RUSTFLAGS");
    cmd.env_remove("CARGO_ENCODED_RUSTFLAGS");
}

/// A file cargo reported as the output of a `compiler-artifact` message.
pub struct Artifact {
    pub path: PathBuf,
    /// Whether this is the target's linked executable (a `bin`'s ELF).
    pub executable: bool,
}

/// The artifacts in one line of `--message-format json` output; empty for
/// anything but a `compiler-artifact` message.
fn artifacts_in(line: &str) -> Vec<Artifact> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if msg["reason"].as_str() != Some("compiler-artifact") {
        return Vec::new();
    }
    let executable = msg["executable"].as_str();
    msg["filenames"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|path| Artifact {
            path: PathBuf::from(path),
            executable: executable == Some(path),
        })
        .collect()
}

/// Whether the `release:` line of `cargo -vV` output names a nightly or dev
/// build, e.g. `release: 1.99.0-nightly`.
fn release_is_nightly(version: &str) -> bool {
    version
        .lines()
        .find_map(|line| line.strip_prefix("release: "))
        .is_some_and(|release| {
            let release = release.trim();
            release.ends_with("-nightly") || release.ends_with("-dev")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_is_nightly_reads_the_channel_from_the_release_line() {
        let version = |release: &str| {
            format!(
                "cargo 1.99.0 (abc 2026-08-06)\nrelease: {release}\nhost: x86_64-unknown-linux-gnu\n"
            )
        };
        assert!(release_is_nightly(&version("1.99.0-nightly")));
        assert!(release_is_nightly(&version("1.99.0-dev")));
        assert!(!release_is_nightly(&version("1.98.1")));
        assert!(!release_is_nightly(&version("1.98.0-beta.3")));
        assert!(!release_is_nightly(
            "cargo 1.99.0-nightly (abc 2026-08-06)\n"
        ));
    }

    #[test]
    fn artifacts_in_flags_the_executable_among_the_filenames() {
        let line = r#"{"reason":"compiler-artifact","target":{"kind":["bin"]},"filenames":["/t/deps/fw-abc.d","/t/release/fw"],"executable":"/t/release/fw"}"#;
        let artifacts = artifacts_in(line);
        let seen: Vec<(&str, bool)> = artifacts
            .iter()
            .map(|a| (a.path.to_str().unwrap(), a.executable))
            .collect();
        assert_eq!(
            seen,
            vec![("/t/deps/fw-abc.d", false), ("/t/release/fw", true)]
        );
    }

    #[test]
    fn artifacts_in_ignores_other_messages() {
        assert!(artifacts_in(r#"{"reason":"build-script-executed","package_id":"x"}"#).is_empty());
        assert!(artifacts_in("not json").is_empty());
    }

    #[test]
    fn set_target_rustflags_clears_the_flags_that_take_precedence() {
        let mut cmd = Command::new("cargo");
        cmd.env("RUSTFLAGS", "-Cinstrument-coverage")
            .env("CARGO_ENCODED_RUSTFLAGS", "-Cinstrument-coverage");
        set_target_rustflags(&mut cmd, "TARGET_RUSTFLAGS", "-C lto");

        let envs: Vec<(&str, Option<&str>)> = cmd
            .get_envs()
            .map(|(key, value)| (key.to_str().unwrap(), value.and_then(|v| v.to_str())))
            .collect();
        assert_eq!(
            envs,
            vec![
                ("CARGO_ENCODED_RUSTFLAGS", None),
                ("RUSTFLAGS", None),
                ("TARGET_RUSTFLAGS", Some("-C lto")),
            ]
        );
    }
}
