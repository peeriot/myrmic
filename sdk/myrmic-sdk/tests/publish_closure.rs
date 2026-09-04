//! Guards the invariant this crate's crates.io publication depends on: the whole
//! path-dependency closure reachable from `myrmic-sdk` must stay publishable. A git
//! source, a version-less path dependency, a missing `license`/`license-file`, a
//! missing `description`, `publish = false`, or a stripped licence/attribution file
//! all silently break `cargo publish` and are otherwise only caught by a human
//! running it at release time.
//!
//! `cargo metadata` is used directly instead of `cargo publish --dry-run` so this
//! runs offline, with no compilation and no registry credentials.

use std::collections::{BTreeSet, VecDeque};
use std::process::Command;

use serde_json::{Value, json};

/// The published closure. Every crate reachable from `myrmic-sdk` other than these
/// is a crates.io registry dependency, which cannot itself carry a git or path
/// source. Checked against the actual dependency graph below, so this list cannot
/// silently drift from reality.
const PUBLISHED_CRATES: &[&str] = &[
    "myrmic-sdk",
    "myrmic-common",
    "myrmic-sdk-macros",
    "myrmic-signal-layer-types",
];

#[test]
fn the_published_closure_is_exactly_the_local_path_dependency_closure() {
    let metadata = cargo_metadata!();
    let expected: BTreeSet<String> = PUBLISHED_CRATES.iter().map(ToString::to_string).collect();

    assert_eq!(
        local_path_closure(&metadata, "myrmic-sdk"),
        expected,
        "the local path-dependency closure reachable from `myrmic-sdk` no longer \
         matches `PUBLISHED_CRATES`; a crate was added to or removed from the \
         publish closure without updating the expected set, which would silently \
         break the publish order"
    );
}

#[test]
fn local_closure_skips_versionless_dev_path_dependencies_but_not_versioned_ones() {
    // A synthetic document rather than the real workspace: it isolates the one
    // behaviour under test (which edges the BFS follows) from everything else
    // `cargo metadata` happens to report, and reproduces the exact shape the
    // reviewer's `myrmic-common -> test-framework` seed exploited (a version-less
    // dev path dependency ballooning the closure).
    let metadata = json!({
        "packages": [
            {
                "name": "root",
                "dependencies": [
                    { "name": "normal-dep", "source": Value::Null, "req": "*", "kind": Value::Null },
                    { "name": "versionless-dev-dep", "source": Value::Null, "req": "*", "kind": "dev" },
                    { "name": "versioned-dev-dep", "source": Value::Null, "req": "^1.0.0", "kind": "dev" },
                ],
            },
            { "name": "normal-dep", "dependencies": [] },
            { "name": "versionless-dev-dep", "dependencies": [] },
            { "name": "versioned-dev-dep", "dependencies": [] },
        ],
    });

    assert_eq!(
        local_path_closure(&metadata, "root"),
        BTreeSet::from([
            "root".to_owned(),
            "normal-dep".to_owned(),
            "versioned-dev-dep".to_owned(),
        ]),
        "the closure must skip a version-less dev path dependency (cargo strips it \
         from the packaged manifest, so its target need not be publishable) while \
         still following a versioned one (cargo keeps that in the packaged \
         manifest, so its target must be publishable)"
    );
}

#[test]
fn every_published_crate_has_the_metadata_cargo_publish_hard_requires() {
    let metadata = cargo_metadata!();

    for crate_name in PUBLISHED_CRATES {
        let package = find_package(&metadata, crate_name);

        // `publish` is `null` when a crate is unrestricted and `[]` (not `false`)
        // when `publish = false`; `cargo metadata` never emits a boolean here.
        assert!(
            package["publish"].is_null(),
            "`{crate_name}` is not unrestricted for publishing (`cargo metadata` \
             reports `\"publish\": {}`); `cargo publish` refuses to publish it \
             without an explicit `publish = true`",
            package["publish"]
        );
        assert!(
            package["description"].as_str().is_some(),
            "`{crate_name}` has no `description`; `cargo publish` hard-refuses to \
             publish without one"
        );
    }
}

#[test]
fn every_published_crate_declares_a_license() {
    let metadata = cargo_metadata!();

    for crate_name in PUBLISHED_CRATES {
        let package = find_package(&metadata, crate_name);

        assert!(
            package["license"].as_str() == Some("MIT OR Apache-2.0")
                || package["license_file"].as_str().is_some(),
            "`{crate_name}` must declare `license = \"MIT OR Apache-2.0\"` or a \
             `license-file` to publish to crates.io"
        );
    }
}

#[test]
fn every_published_crate_packages_its_license_and_third_party_attribution() {
    for crate_name in PUBLISHED_CRATES {
        let files = packaged_files!(crate_name);

        for licence_file in ["LICENSE-MIT", "LICENSE-APACHE"] {
            assert!(
                files.contains(licence_file),
                "`{crate_name}`'s packaged file list (`cargo package --list`) does not \
                 contain `{licence_file}`; the `include` list stripped the crate's own \
                 licence text, and publishing it would be irreversible"
            );
        }
    }

    let common_files = packaged_files!("myrmic-common");

    for attribution_file in ["NOTICE", "LICENSES/MIT-nourl.txt", "LICENSES/MIT-http.txt"] {
        assert!(
            common_files.contains(attribution_file),
            "`myrmic-common`'s packaged file list (`cargo package --list`) does not \
             contain `{attribution_file}`; the `include` list stripped the required \
             MIT attribution for the third-party code vendored under \
             `src/types/web`, and publishing it would be irreversible"
        );
    }
}

#[test]
fn no_published_crate_has_a_git_or_versionless_path_dependency() {
    let metadata = cargo_metadata!();

    for (crate_name, dependency) in published_dependencies(&metadata) {
        let dependency_name = dependency["name"]
            .as_str()
            .expect("a dependency always has a name");
        let source = dependency["source"].as_str();
        let requirement = dependency["req"]
            .as_str()
            .expect("a dependency always has a version requirement");
        let is_dev = dependency["kind"].as_str() == Some("dev");

        match (is_dev, source) {
            // A dev-dependency on a local path crate exists only to close a
            // doc/test cycle (`myrmic-sdk-macros` on `myrmic-sdk`) and must stay
            // version-less: cargo strips a version-less path dev-dependency from
            // the packaged manifest, but keeps a versioned one, which would then
            // require the target already published under that exact version -
            // impossible for crates the closure publishes together for the
            // first time.
            (true, None) => assert_eq!(
                requirement, "*",
                "`{crate_name}` has a dev-dependency on local path crate \
                 `{dependency_name}` with a version (`{requirement}`); cargo keeps \
                 a versioned dev path dependency in the packaged manifest, \
                 requiring `{dependency_name}` to already be published under \
                 that version"
            ),
            (false, None) => assert_ne!(
                requirement, "*",
                "`{crate_name}` depends on local path crate `{dependency_name}` with \
                 no `version =`; crates.io refuses to publish a version-less path \
                 dependency"
            ),
            (_, Some(source)) => assert!(
                !source.starts_with("git+"),
                "`{crate_name}` depends on `{dependency_name}` from a git source \
                 ({source}); crates.io refuses to publish a git-sourced dependency"
            ),
        }
    }
}

#[test]
fn every_local_path_dependency_requires_the_current_workspace_version() {
    let metadata = cargo_metadata!();
    let workspace_version = find_package(&metadata, "myrmic-sdk")["version"]
        .as_str()
        .expect("a package always has a version")
        .to_owned();
    let expected_requirement = format!("^{workspace_version}");

    for (crate_name, dependency) in published_dependencies(&metadata) {
        let requirement = dependency["req"]
            .as_str()
            .expect("a dependency always has a version requirement");

        // Only a versioned local path dependency carries a hand-written version
        // literal that can go stale; a registry dependency has its own
        // independent version and a version-less path dependency has no literal
        // to drift.
        if !dependency["source"].is_null() || requirement == "*" {
            continue;
        }

        let dependency_name = dependency["name"]
            .as_str()
            .expect("a dependency always has a name");

        assert_eq!(
            requirement, expected_requirement,
            "`{crate_name}`'s path dependency on `{dependency_name}` requires \
             `{requirement}`, which no longer matches the workspace version \
             `{workspace_version}`; a release-prep version bump must update every \
             hand-written path-dependency version alongside \
             `[workspace.package].version`"
        );
    }
}

/// Runs `cargo metadata` for the whole workspace and returns the parsed document,
/// asserting at the call site that the subprocess actually succeeded so a plumbing
/// failure names the failing test instead of this macro.
///
/// `--offline --locked` keep this deterministic: no network call, and a stale
/// `Cargo.lock` fails the test instead of being silently rewritten. `--no-deps`
/// keeps this to the workspace's own manifests: with the full graph, `cargo
/// metadata --offline` also has to resolve and read every dependency crate
/// (including riscv32/esp32/wasm-only packages a host-only CI job never fetches),
/// which fails on a cold runner with a download error unrelated to
/// publishability. It also scopes `packages` to workspace members, so a package
/// name lookup here can never resolve to a same-named crate already published to
/// the registry. No `--manifest-path` is needed: `cargo metadata` already walks up
/// from the test binary's working directory (this crate's manifest directory) to
/// find the workspace root.
#[macro_export]
macro_rules! cargo_metadata {
    () => {{
        let output = Command::new(env!("CARGO"))
            .args([
                "metadata",
                "--format-version",
                "1",
                "--offline",
                "--locked",
                "--no-deps",
            ])
            .output()
            .expect("failed to run `cargo metadata`");

        assert!(
            output.status.success(),
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        serde_json::from_slice::<Value>(&output.stdout)
            .expect("cargo metadata did not print valid JSON")
    }};
}

/// Runs `cargo package --list` for `crate_name` and returns its packaged file set,
/// asserting at the call site that the subprocess actually succeeded so a
/// plumbing failure names the failing test instead of this macro.
///
/// Derives the packaged file set from what `cargo package` actually decides to
/// ship rather than re-reading a manifest's `include` literal, which is exactly
/// the thing that can drift from it (an over-tight or mismatched glob). `--offline`
/// keeps this network-free; `--allow-dirty` is needed because the workspace tree
/// is not guaranteed to be git-clean while this test runs.
#[macro_export]
macro_rules! packaged_files {
    ($crate_name:expr) => {{
        let crate_name: &str = $crate_name;
        let output = Command::new(env!("CARGO"))
            .args([
                "package",
                "--list",
                "--offline",
                "--allow-dirty",
                "-p",
                crate_name,
            ])
            .output()
            .expect("failed to run `cargo package --list`");

        assert!(
            output.status.success(),
            "`cargo package --list -p {crate_name}` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<String>>()
    }};
}

/// Looks up a workspace package by name in an already-fetched `cargo metadata`
/// document. `metadata["packages"]` only ever holds workspace members here
/// because every caller fetches it with `--no-deps`, so this can never resolve a
/// same-named crate pulled in from the registry.
fn find_package<'a>(metadata: &'a Value, crate_name: &str) -> &'a Value {
    metadata["packages"]
        .as_array()
        .expect("`cargo metadata` always reports a `packages` array")
        .iter()
        .find(|package| package["name"] == *crate_name)
        .unwrap_or_else(|| panic!("published crate `{crate_name}` not found in workspace metadata"))
}

/// Every workspace crate path-reachable from `start`, `start` itself included: the
/// set that must all be publishable, in dependency order, for `cargo publish -p
/// start` to eventually succeed. A dependency is "local" when `cargo metadata`
/// reports no `source` for it, which is exactly how it encodes a path dependency.
///
/// A version-less dev path dependency is not followed: cargo strips it from the
/// packaged manifest, so its target need not be publishable. A *versioned* dev
/// path dependency is followed, since cargo keeps that one in the packaged
/// manifest and its target must be publishable too.
fn local_path_closure(metadata: &Value, start: &str) -> BTreeSet<String> {
    let mut closure = BTreeSet::new();
    let mut queue = VecDeque::from([start.to_owned()]);

    while let Some(crate_name) = queue.pop_front() {
        if !closure.insert(crate_name.clone()) {
            continue;
        }

        let dependencies = find_package(metadata, &crate_name)["dependencies"]
            .as_array()
            .expect("`cargo metadata` always reports a `dependencies` array");

        for dependency in dependencies {
            let is_versionless_dev_dependency = dependency["kind"].as_str() == Some("dev")
                && dependency["req"].as_str() == Some("*");

            if dependency["source"].is_null() && !is_versionless_dev_dependency {
                let name = dependency["name"]
                    .as_str()
                    .expect("a dependency always has a name");
                queue.push_back(name.to_owned());
            }
        }
    }

    closure
}

/// Every `(crate_name, dependency)` pair across the published closure, flattening
/// the nested loop the git/versionless and version-alias checks both walk.
fn published_dependencies(metadata: &Value) -> Vec<(&str, &Value)> {
    PUBLISHED_CRATES
        .iter()
        .flat_map(|&crate_name| {
            find_package(metadata, crate_name)["dependencies"]
                .as_array()
                .expect("`cargo metadata` always reports a `dependencies` array")
                .iter()
                .map(move |dependency| (crate_name, dependency))
        })
        .collect()
}
