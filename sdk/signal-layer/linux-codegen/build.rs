//! Build script: rebuild this crate whenever a signal-module descriptor is added, removed, or
//! changed.
//!
//! The generator embeds every `signal-modules/{drivers,steps}/*/descriptor.yaml` at compile time
//! via `include_dir!`. Those files are not otherwise Cargo build inputs, so without the
//! `rerun-if-changed` lines emitted here a cached build of this crate keeps a stale copy of the
//! module tree, and codegen then fails with `descriptor: No such file or directory` after a new
//! module is added.

use std::path::Path;

fn main() {
    // The build script runs before the module tree is embedded, so it must point at the same
    // directories as the `include_dir!` calls in `src/lib.rs`.
    let signal_modules = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("signal-modules");

    for group in ["drivers", "steps"] {
        let root = signal_modules.join(group);

        // The group directory itself, so that adding or removing a module dir re-runs this script
        // and rebuilds the crate.
        println!("cargo:rerun-if-changed={}", root.display());

        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // The module directory (a descriptor added or removed within it) and the descriptor's
            // contents (an existing descriptor edited).
            println!("cargo:rerun-if-changed={}", path.display());
            let descriptor = path.join("descriptor.yaml");
            if descriptor.exists() {
                println!("cargo:rerun-if-changed={}", descriptor.display());
            }
        }
    }
}
