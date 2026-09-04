use std::path::Path;
use std::process::Command;

/// Captures the source revision this CLI is built from and exposes it to the
/// crate as two env vars:
///
/// - `MYRMIC_VERSION` — `<crate version> (<hash|"unknown">)`, used for `--version`.
/// - `MYRMIC_GIT_HASH` — the bare hash, set only when a revision was found, so
///   `option_env!` is `None` on a VCS-less build and `myrmic new` can demand
///   an explicit `--sdk` instead of pinning to a guessed revision.
///
/// `MYRMIC_SDK_VERSION` is read by the crate itself, not re-exported; it only
/// matters here because a release build with it set has no need of a revision.
fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let hash = git_hash().or_else(jj_hash);

    match hash.as_deref() {
        Some(hash) => println!("cargo::rustc-env=MYRMIC_GIT_HASH={hash}"),
        None if std::env::var_os("MYRMIC_SDK_VERSION").is_some() => {}
        None => println!(
            "cargo::warning=myrmic-cli: no git/jj revision detected; `myrmic new` will require --sdk"
        ),
    }
    println!(
        "cargo::rustc-env=MYRMIC_VERSION={version} ({})",
        hash.as_deref().unwrap_or("unknown"),
    );

    rerun_on_vcs_head();
    println!("cargo::rerun-if-env-changed=PEERIOT_MYRMIC_SDK");
    println!("cargo::rerun-if-env-changed=MYRMIC_SDK_VERSION");
}

/// `git rev-parse --short=8 HEAD` — works on a plain git checkout or a
/// git-colocated jj repo (CI, releases).
fn git_hash() -> Option<String> {
    run(Command::new("git").args(["rev-parse", "--short=8", "HEAD"]))
}

/// The commit under the jj working copy (`@-`) — the last non-working-copy
/// revision, matching what git reports as HEAD in a colocated repo. Used when
/// there is no colocated git, e.g. a bare jj worktree.
fn jj_hash() -> Option<String> {
    run(Command::new("jj").args([
        "--no-pager",
        "--ignore-working-copy",
        "log",
        "--no-graph",
        "-r",
        "@-",
        "-T",
        "commit_id.short(8)",
    ]))
}

fn run(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let hash = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!hash.is_empty()).then_some(hash)
}

/// Re-run the build script when the checked-out revision moves, so the embedded
/// hash stays current across commits without a `cargo clean`. Only emits paths
/// that exist to avoid spurious rebuilds.
fn rerun_on_vcs_head() {
    let mut dir = std::env::current_dir().ok();
    while let Some(cur) = dir {
        for rel in [".git/HEAD", ".jj/working_copy/checkout"] {
            let path = cur.join(rel);
            if path.exists() {
                println!("cargo::rerun-if-changed={}", path.display());
            }
        }
        dir = cur.parent().map(Path::to_path_buf);
    }
}
