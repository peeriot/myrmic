//! Compiling a cell logic crate to a `wasm32-unknown-unknown` module.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;
use which::which;

use crate::CargoTarget;
use crate::cargo;

const TARGET: &str = "wasm32-unknown-unknown";
const BUILD_STD: &str = "core,alloc,compiler_builtins";

const DEFAULT_STACK_SIZE: usize = 32 * 1024;
const DEFAULT_INITIAL_MEMORY: usize = 64 * 1024;
const DEFAULT_MAX_MEMORY: usize = 64 * 1024;
// This is duplicated in the myrmic_sdk as well, but we can't easily get the constant to both crates.
const DEFAULT_HEAP_SIZE: usize = 32 * 1024;

const PAGE_SIZE: usize = 65_536;

const RUSTFLAGS_ENV: &str = "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS";
const HEAP_SIZE_ENV: &str = "WASM_SDK_HEAP_SIZE";

/// Per-cell memory layout, sourced from `[package.metadata.myrmic]` (with
/// fallbacks to the defaults). Values are in bytes.
#[derive(Clone, Copy, Deserialize)]
#[serde(default)]
struct MemoryConfig {
    heap_size: usize,
    stack_size: usize,
    initial_memory: usize,
    max_memory: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            heap_size: DEFAULT_HEAP_SIZE,
            stack_size: DEFAULT_STACK_SIZE,
            initial_memory: DEFAULT_INITIAL_MEMORY,
            max_memory: DEFAULT_MAX_MEMORY,
        }
    }
}

impl MemoryConfig {
    pub fn adjust(&mut self) {
        let total = self.heap_size + self.stack_size;
        if total > self.initial_memory {
            self.initial_memory = total;
        }
        if total > self.max_memory {
            self.max_memory = total;
        }

        self.initial_memory = ((self.initial_memory / PAGE_SIZE) + 1) * PAGE_SIZE;
        self.max_memory = ((self.max_memory / PAGE_SIZE) + 1) * PAGE_SIZE;
    }
}

/// The concrete cargo target a [`CargoTarget`] selection resolves to.
#[derive(Debug)]
enum Selector {
    Lib,
    Bin(String),
}

/// Cargo metadata reports every library kind (`lib`, `rlib`, `cdylib`, …) as a
/// distinct target kind; treat any of them as "the library".
fn is_lib_kind(kind: &str) -> bool {
    matches!(
        kind,
        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
    )
}

/// Resolve a [`CargoTarget`] selection against the crate's declared targets,
/// erroring with the available options when the choice is ambiguous or missing.
fn resolve_selector(manifest_path: &Path, cargo_target: &CargoTarget) -> anyhow::Result<Selector> {
    let targets = cargo::package_targets(manifest_path)?;

    let bins: Vec<&str> = targets
        .iter()
        .filter(|t| t.kinds.iter().any(|k| k == "bin"))
        .map(|t| t.name.as_str())
        .collect();
    let libs: Vec<&str> = targets
        .iter()
        .filter(|t| t.kinds.iter().any(|k| is_lib_kind(k)))
        .map(|t| t.name.as_str())
        .collect();

    select(&bins, &libs, cargo_target)
        .with_context(|| format!("resolving build target for `{}`", manifest_path.display()))
}

/// Pick a concrete target from a crate's declared `bins`/`libs`.
///
/// `Auto` prefers the sole binary and falls back to the sole library, but
/// refuses to guess when a crate declares more than one binary — the choice is
/// ambiguous, so the caller must name one (`--target <name>` or `lib`). A
/// `Named` selector matches a binary first, then the library.
fn select(bins: &[&str], libs: &[&str], cargo_target: &CargoTarget) -> anyhow::Result<Selector> {
    match cargo_target {
        CargoTarget::Lib => {
            if libs.is_empty() {
                anyhow::bail!(
                    "no library target; name a binary instead, e.g. `--target {}`",
                    bins.first().copied().unwrap_or("<name>"),
                );
            }
            Ok(Selector::Lib)
        }
        CargoTarget::Named(name) => {
            if bins.contains(&name.as_str()) {
                Ok(Selector::Bin(name.clone()))
            } else if libs.contains(&name.as_str()) {
                Ok(Selector::Lib)
            } else {
                anyhow::bail!(
                    "no target `{name}`; available binaries: [{}], libraries: [{}]",
                    bins.join(", "),
                    libs.join(", "),
                );
            }
        }
        CargoTarget::Auto => match bins {
            [only] => Ok(Selector::Bin((*only).to_owned())),
            [] if libs.len() == 1 => Ok(Selector::Lib),
            [] => anyhow::bail!("no binary or library target to build"),
            many => anyhow::bail!(
                "multiple binaries ([{}]) make the target ambiguous; name one with \
                 `--target <name>` (or `target:` in app_specs)",
                many.join(", "),
            ),
        },
    }
}

/// Compiles the cell logic crate at `manifest_path` to wasm and returns the
/// produced wasm artifact paths.
pub(crate) fn compile_cell(
    manifest_path: &Path,
    cargo_target: &CargoTarget,
) -> anyhow::Result<Vec<PathBuf>> {
    ensure_c_linker()?;

    let mut memory: MemoryConfig =
        cargo::read_package_metadata(manifest_path)?.with_context(|| {
            format!(
                "manifest is a workspace, not a cell: {}",
                manifest_path.display()
            )
        })?;

    memory.adjust();

    let selector = resolve_selector(manifest_path, cargo_target)?;

    let manifest_arg = manifest_path
        .to_str()
        .context("manifest path is not valid UTF-8")?;

    let build_std = format!("build-std={BUILD_STD}");

    let mut cmd = Command::new("cargo");
    cmd.args([
        "+nightly-2026-08-07",
        "rustc",
        "--release",
        "--target",
        TARGET,
        "-Z",
        &build_std,
        "--manifest-path",
        manifest_arg,
    ]);

    // A library must be linked as a `cdylib` to emit a standalone wasm module
    // (the manifest declares `rlib`); a binary already links to one.
    match &selector {
        Selector::Lib => {
            cmd.args(["--lib", "--crate-type", "cdylib"]);
        }
        Selector::Bin(name) => {
            cmd.args(["--bin", name]);
        }
    }

    cmd.env(HEAP_SIZE_ENV, memory.heap_size.to_string());
    cmd.env(RUSTFLAGS_ENV, rustflags(&memory));
    cmd.env_remove("RUSTUP_TOOLCHAIN");

    let mut artifacts = Vec::new();

    cargo::process_cargo_build(cmd, |artifact_path| {
        let ext = Path::new(artifact_path).extension();
        if ext.is_some_and(|ext| ext.eq_ignore_ascii_case("wasm")) {
            artifacts.push(PathBuf::from(artifact_path));
        }
    })?;

    Ok(artifacts)
}

/// Fail before the cargo build runs when nothing on this machine can link a
/// host binary: a cell targets wasm32, but cargo still links every dependency's
/// build script for this machine.
fn ensure_c_linker() -> anyhow::Result<()> {
    if which("cc").is_ok() {
        return Ok(());
    }

    // The override cargo honours is named after the host triple, which only rustc knows.
    let hatch = Command::new("rustc")
        .arg("-vV")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|version| host_linker_var(&version));

    if hatch
        .as_deref()
        .is_some_and(|name| linker_configured_by_env(env::vars_os(), name))
    {
        return Ok(());
    }

    let (hatch, hint) = match hatch.as_deref() {
        Some(name) => (name, ""),
        None => (
            "CARGO_TARGET_<HOST_TRIPLE>_LINKER",
            " (resolve <HOST_TRIPLE> with `rustc -vV | sed -n 's/^host: //p'`, uppercased with \
             `-` replaced by `_`)",
        ),
    };

    anyhow::bail!(
        "no C linker found: `cc` is not on PATH, and building a cell links build scripts for \
         this machine - install a C toolchain (`sudo apt install build-essential` on \
         Debian/Ubuntu, `sudo dnf install gcc` on RHEL/Fedora), or set {hatch} to your \
         compiler driver{hint}"
    )
}

/// The `CARGO_TARGET_<HOST_TRIPLE>_LINKER` variable cargo honours for this
/// machine's build scripts.
fn host_linker_var(rustc_version: &str) -> Option<String> {
    let host = rustc_version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))?
        .trim();

    Some(format!(
        "CARGO_TARGET_{}_LINKER",
        host.to_ascii_uppercase().replace('-', "_")
    ))
}

/// Whether `vars` sets `name` to a non-empty value.
fn linker_configured_by_env<TVars>(vars: TVars, name: &str) -> bool
where
    TVars: IntoIterator<Item = (OsString, OsString)>,
{
    vars.into_iter()
        .any(|(var, value)| var == name && !value.is_empty())
}

fn rustflags(memory: &MemoryConfig) -> String {
    let stack_size = memory.stack_size;
    let initial_memory = memory.initial_memory;
    let max_memory = memory.max_memory;

    [
        "-C link-arg=--stack-first".to_owned(),
        format!("-C link-arg=-zstack-size={stack_size}"),
        format!("-C link-arg=--initial-memory={initial_memory}"),
        format!("-C link-arg=--max-memory={max_memory}"),
        "-C target-cpu=mvp".to_owned(),
        "-C opt-level=z".to_owned(),
        "-C lto".to_owned(),
        "-C embed-bitcode=yes".to_owned(),
        "-C codegen-units=1".to_owned(),
        "-C panic=abort".to_owned(),
    ]
    .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(bins: &[&str], libs: &[&str]) -> anyhow::Result<Selector> {
        select(bins, libs, &CargoTarget::Auto)
    }

    #[test]
    fn auto_prefers_the_sole_binary_over_the_lib() {
        assert!(matches!(auto(&["srv"], &["lib"]).unwrap(), Selector::Bin(n) if n == "srv"));
    }

    #[test]
    fn auto_falls_back_to_the_lib_when_there_are_no_bins() {
        assert!(matches!(auto(&[], &["lib"]).unwrap(), Selector::Lib));
    }

    #[test]
    fn auto_errors_on_multiple_bins_rather_than_picking_the_lib() {
        let err = auto(&["parent", "child"], &["lib"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("ambiguous"), "{err}");
        assert!(err.contains("parent") && err.contains("child"), "{err}");
    }

    #[test]
    fn auto_errors_when_there_is_nothing_to_build() {
        assert!(auto(&[], &[]).is_err());
    }

    #[test]
    fn named_selects_the_matching_bin_among_many() {
        let s = select(
            &["parent", "child"],
            &["lib"],
            &CargoTarget::Named("child".into()),
        )
        .unwrap();
        assert!(matches!(s, Selector::Bin(n) if n == "child"));
    }

    #[test]
    fn named_resolves_to_the_lib_when_no_bin_matches() {
        let s = select(&[], &["my_cell"], &CargoTarget::Named("my_cell".into())).unwrap();
        assert!(matches!(s, Selector::Lib));
    }

    #[test]
    fn named_prefers_a_bin_over_a_lib_of_the_same_name() {
        let s = select(&["dup"], &["dup"], &CargoTarget::Named("dup".into())).unwrap();
        assert!(matches!(s, Selector::Bin(n) if n == "dup"));
    }

    #[test]
    fn named_rejects_an_unknown_name() {
        assert!(select(&["parent"], &[], &CargoTarget::Named("nope".into())).is_err());
    }

    #[test]
    fn explicit_lib_requires_a_lib_target() {
        assert!(matches!(
            select(&["a", "b"], &["l"], &CargoTarget::Lib).unwrap(),
            Selector::Lib
        ));
        assert!(select(&["a"], &[], &CargoTarget::Lib).is_err());
    }

    #[test]
    fn only_the_host_targets_linker_var_counts_as_configured() {
        const HOST_LINKER_VAR: &str = "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER";

        let cases: &[(&[(&str, &str)], bool)] = &[
            (
                &[("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER", "clang")],
                true,
            ),
            (
                &[("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER", "")],
                false,
            ),
            (
                &[("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER", "cc")],
                false,
            ),
            (
                &[("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_LINKER", "rust-lld")],
                false,
            ),
            // Would pass if matching became suffix-based instead of an exact name match.
            (&[("MY_LINKER", "clang")], false),
            // Neither of these reaches the host build-script link.
            (&[("CC", "clang")], false),
            (&[("RUSTFLAGS", "-Clinker=clang")], false),
            (&[], false),
            (&[("PATH", "/usr/bin"), ("HOME", "/root")], false),
        ];

        for (vars, expected) in cases {
            let owned: Vec<(OsString, OsString)> = vars
                .iter()
                .map(|(name, value)| (OsString::from(*name), OsString::from(*value)))
                .collect();
            assert_eq!(
                linker_configured_by_env(owned, HOST_LINKER_VAR),
                *expected,
                "{vars:?}"
            );
        }
    }

    #[test]
    fn host_linker_var_uppercases_the_triple_and_underscores_its_dashes() {
        let version = "rustc 1.98.1 (abcdef 2026-01-01)\nbinary: rustc\nhost: x86_64-unknown-linux-gnu\nrelease: 1.98.1\n";

        assert_eq!(
            host_linker_var(version).as_deref(),
            Some("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER")
        );
        assert_eq!(host_linker_var("rustc 1.98.1\n"), None);
    }

    fn memory_config_from(manifest: &str) -> MemoryConfig {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        std::fs::write(&path, manifest).unwrap();
        cargo::read_package_metadata(&path).unwrap().unwrap()
    }

    #[test]
    fn missing_myrmic_table_yields_the_defaults_not_zeros() {
        // A cell with no `[package.metadata.myrmic]` must still get the real
        // defaults — a zeroed heap bakes `WASM_SDK_HEAP_SIZE=0` into the module
        // and traps in `init_allocator`.
        let memory = memory_config_from("[package]\nname = \"c\"\nversion = \"0.1.0\"\n");
        assert_eq!(memory.heap_size, DEFAULT_HEAP_SIZE);
        assert_eq!(memory.stack_size, DEFAULT_STACK_SIZE);
        assert_eq!(memory.initial_memory, DEFAULT_INITIAL_MEMORY);
        assert_eq!(memory.max_memory, DEFAULT_MAX_MEMORY);
    }

    #[test]
    fn partial_myrmic_table_fills_omitted_fields_with_defaults() {
        let memory = memory_config_from(
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\n\
             [package.metadata.myrmic]\nheap_size = 2048\n",
        );
        assert_eq!(memory.heap_size, 2048);
        assert_eq!(memory.stack_size, DEFAULT_STACK_SIZE);
        assert_eq!(memory.initial_memory, DEFAULT_INITIAL_MEMORY);
        assert_eq!(memory.max_memory, DEFAULT_MAX_MEMORY);
    }
}
