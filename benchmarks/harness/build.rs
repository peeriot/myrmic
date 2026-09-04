//! Bakes the current git commit into `BENCH_GIT_SHA`, so every report this harness produces
//! carries an identifier of the code that produced it — see `bench_report::raw::MultiRunReport`'s
//! `version` field and [`bench_report::raw::MultiRunReport::merge`], which refuses to combine
//! reports whose stamps don't match.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=BENCH_GIT_SHA={sha}");
    // best-effort: catches switching commits on the same branch, not every way HEAD can change.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
