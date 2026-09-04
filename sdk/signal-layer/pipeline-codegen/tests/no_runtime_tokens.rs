//! SR-5 grep gate: no runtime-family token literals may appear inside `quote!`
//! or `quote_spanned!` blocks in the seven `emit/*.rs` source files.
//!
//! After the Task 9 refactor, every `embassy_*`, `tokio::`, `static_cell`,
//! `NoopRawMutex`, and `wasm_runtime::` token is emitted only by
//! `pipeline-backend-api`'s scaffold functions (called through `ChipBackend`
//! hooks), never inline in the chip-agnostic emitter.
//!
//! Implementation note: the simplest and most conservative gate is to scan
//! for these strings anywhere in the emit source files (not just inside
//! `quote!` blocks). Because the emit files import and call hook functions
//! whose names do not contain these strings, this search is both sound and
//! complete: any regression that re-introduces a runtime literal in a
//! `quote!` block will also appear as a plain string in the file.

use std::path::Path;

/// The seven emit files owned by Task 9.
const EMIT_FILES: &[&str] = &[
    "src/emit/imports.rs",
    "src/emit/source_task.rs",
    "src/emit/spawn.rs",
    "src/emit/taps.rs",
    "src/emit/buses.rs",
    "src/emit/sink_task.rs",
    "src/emit/outlets.rs",
];

/// Prohibited string patterns — these are the runtime-family token literals
/// that must live only in `pipeline-backend-api`'s scaffold modules.
const PROHIBITED: &[&str] = &[
    "embassy_",
    "tokio::",
    "static_cell",
    "NoopRawMutex",
    "wasm_runtime::",
    "SpiDevice",
    "I2cDevice",
    "Mutex::",
];

fn crate_root() -> &'static Path {
    // The test binary is compiled from the crate root; CARGO_MANIFEST_DIR
    // is set by Cargo to the crate's Cargo.toml directory.
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn no_runtime_token_literals_in_emit_files() {
    let mut violations: Vec<String> = Vec::new();

    for relative_path in EMIT_FILES {
        let path = crate_root().join(relative_path);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));

        for pattern in PROHIBITED {
            if source.contains(pattern) {
                // Report every matching line for clear diagnostics.
                for (line_no, line) in source.lines().enumerate() {
                    if line.contains(pattern) {
                        violations.push(format!(
                            "{}:{}: found `{pattern}` in: {line}",
                            relative_path,
                            line_no + 1,
                        ));
                    }
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Runtime-family token literals found in emit/*.rs files (SR-5 violation).\n\
         These must live only in pipeline-backend-api scaffold modules, not inline \
         in the chip-agnostic emitter.\n\nViolations:\n{}",
        violations.join("\n")
    );
}
