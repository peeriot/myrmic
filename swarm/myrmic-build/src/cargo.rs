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
    }

    let out = cmd.output()?;

    if !out.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
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
    }

    let out = cmd.output()?;

    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).into_owned())
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
    F: for<'a> FnMut(&'a str),
{
    cmd.arg("--message-format").arg("json-render-diagnostics");

    cmd.stdout(Stdio::piped());
    // stderr is inherited so cargo's rendered diagnostics and progress reach the terminal (so the user can see it).
    let mut child = cmd.spawn().context("failed to run cargo build")?;
    let stdout = child.stdout.take().expect("stdout was piped");

    for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)) {
        let line = line.context("unable to read cargo output")?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg["reason"].as_str() != Some("compiler-artifact") {
            continue;
        }
        let Some(filenames) = msg["filenames"].as_array() else {
            continue;
        };
        for name in filenames {
            if let Some(path) = name.as_str() {
                func(path);
            }
        }
    }

    let status = child.wait().context("cargo failed to exit")?;
    if !status.success() {
        anyhow::bail!("build failed");
    }

    Ok(())
}
