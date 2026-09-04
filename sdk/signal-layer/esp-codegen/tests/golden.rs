//! Golden-fixture test for esp-codegen.
//!
//! Pins the current generation output byte-for-byte so that the Task 8/9
//! refactor can be proven byte-identical.  Two fixtures are tested:
//!
//!   * `sensors-only`   — BME280 source + moving-average step + retained taps.
//!     Guards: `#[embassy_executor::task]`, `Ticker`, `Instant::now` (`source_task`),
//!     `init_tap_registry` (taps), `StaticCell`/`NoopRawMutex` bus statics (buses).
//!
//!   * `outlet-bearing` — gpio-output sink outlet.
//!     Guards all of the above for the outlet path plus:
//!     `init_outlet_registry` (outlets), sink task, pins macro.
//!
//! # Updating the expected files
//!
//! Run once with `UPDATE_GOLDEN=1` to (re)write the committed expected files:
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo test -p esp-codegen --test golden
//! ```
//!
//! Then re-run WITHOUT the env var to confirm deterministic output:
//!
//! ```text
//! cargo test -p esp-codegen --test golden
//! ```

use std::path::PathBuf;

/// Root of the repository, derived from `CARGO_MANIFEST_DIR`.
fn repo_root() -> PathBuf {
    // Cargo.toml lives at sdk/signal-layer/esp-codegen/
    // so we go up three levels to reach the repo root.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(3)
        .expect("cannot locate repo root from CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn drivers_root() -> PathBuf {
    repo_root().join("signal-modules/drivers")
}

fn steps_root() -> PathBuf {
    repo_root().join("signal-modules/steps")
}

/// Run generation for a named fixture and compare (or write) the expected file.
///
/// `fixture` is the base name without extension (e.g. `"sensors-only"`).
/// The board manifest is `<fixture>.board.yaml` when that file exists, else the
/// shared `golden-board.yaml`. A fixture needs its own board when a device would
/// perturb the other fixtures, since board peripherals are emitted for every
/// device in the manifest regardless of what the pipeline uses.
fn check_fixture(fixture: &str) {
    let per_fixture_board = fixtures_dir().join(format!("{fixture}.board.yaml"));
    let board = if per_fixture_board.exists() {
        per_fixture_board
    } else {
        fixtures_dir().join("golden-board.yaml")
    };
    let pipeline = fixtures_dir().join(format!("{fixture}.yaml"));
    let expected_path = fixtures_dir().join(format!("{fixture}.expected.rs"));

    let actual = esp_codegen::generate_esp32(&board, &pipeline, &drivers_root(), &steps_root())
        .unwrap_or_else(|e| panic!("generation failed for fixture `{fixture}`: {e:#}"));

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&expected_path, &actual).unwrap_or_else(|e| {
            panic!(
                "failed to write expected file {}: {e}",
                expected_path.display()
            )
        });
        eprintln!("UPDATE_GOLDEN: wrote {}", expected_path.display());
        return;
    }

    let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|e| {
        panic!(
            "expected file {} not found — run with UPDATE_GOLDEN=1 first: {e}",
            expected_path.display()
        )
    });

    if actual != expected {
        // Emit a diff-friendly error: show the first differing line.
        let actual_lines: Vec<&str> = actual.lines().collect();
        let expected_lines: Vec<&str> = expected.lines().collect();
        let first_diff = actual_lines
            .iter()
            .zip(expected_lines.iter())
            .enumerate()
            .find(|(_, (a, e))| a != e)
            .map_or_else(
                || format!("lengths differ: {} vs {}", actual.len(), expected.len()),
                |(i, (a, e))| format!("line {}: actual={a:?} expected={e:?}", i + 1),
            );

        panic!(
            "golden output mismatch for fixture `{fixture}`.\n\
             First difference: {first_diff}\n\
             Re-run with UPDATE_GOLDEN=1 to refresh the expected file."
        );
    }
}

#[test]
fn golden_sensors_only() {
    check_fixture("sensors-only");
}

#[test]
fn golden_pwm_outlet() {
    check_fixture("pwm-outlet");
}

#[test]
fn golden_outlet_bearing() {
    check_fixture("outlet-bearing");
}

/// Verify that the sensors-only expected output contains the tokens that
/// Task 8/9 will move behind hooks — so this test guards those refactors.
///
/// Skipped when `UPDATE_GOLDEN=1` is set (expected file may not exist yet) or
/// when the expected file is absent (first run before capture).
#[test]
fn sensors_only_expected_contains_required_tokens() {
    // Skip during UPDATE_GOLDEN: the golden_ tests write the files; token checks
    // read them.  Running both in the same invocation can race.
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        eprintln!("sensors_only_expected_contains_required_tokens: skipped (UPDATE_GOLDEN)");
        return;
    }
    let expected_path = fixtures_dir().join("sensors-only.expected.rs");
    let Ok(src) = std::fs::read_to_string(&expected_path) else {
        eprintln!(
            "sensors_only_expected_contains_required_tokens: skipped (expected file not found — run with UPDATE_GOLDEN=1 first)"
        );
        return;
    };

    // embassy_executor::task — source task attribute
    assert!(
        src.contains("embassy_executor::task"),
        "missing #[embassy_executor::task] in sensors-only expected output"
    );
    // Ticker — interval-based polling in source_task
    assert!(
        src.contains("Ticker"),
        "missing Ticker in sensors-only expected output"
    );
    // Instant::now — timestamp in source_task
    assert!(
        src.contains("Instant::now"),
        "missing Instant::now in sensors-only expected output"
    );
    // init_tap_registry — taps handoff
    assert!(
        src.contains("init_tap_registry"),
        "missing init_tap_registry in sensors-only expected output"
    );
    // StaticCell — bus static
    assert!(
        src.contains("StaticCell"),
        "missing StaticCell in sensors-only expected output"
    );
    // NoopRawMutex — bus mutex type
    assert!(
        src.contains("NoopRawMutex"),
        "missing NoopRawMutex in sensors-only expected output"
    );
}

/// Verify that the outlet-bearing expected output additionally contains tokens
/// from the `sink_task` and outlets emit paths.
///
/// Skipped when `UPDATE_GOLDEN=1` is set (expected file may not exist yet) or
/// when the expected file is absent (first run before capture).
#[test]
fn outlet_bearing_expected_contains_required_tokens() {
    // Skip during UPDATE_GOLDEN: the golden_ tests write the files; token checks
    // read them.  Running both in the same invocation can race.
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        eprintln!("outlet_bearing_expected_contains_required_tokens: skipped (UPDATE_GOLDEN)");
        return;
    }
    let expected_path = fixtures_dir().join("outlet-bearing.expected.rs");
    let Ok(src) = std::fs::read_to_string(&expected_path) else {
        eprintln!(
            "outlet_bearing_expected_contains_required_tokens: skipped (expected file not found — run with UPDATE_GOLDEN=1 first)"
        );
        return;
    };

    // sink task attribute
    assert!(
        src.contains("embassy_executor::task"),
        "missing #[embassy_executor::task] in outlet-bearing expected output"
    );
    // Ticker in sink_task
    assert!(
        src.contains("Ticker"),
        "missing Ticker in outlet-bearing expected output"
    );
    // Instant::now in sink_task
    assert!(
        src.contains("Instant::now"),
        "missing Instant::now in outlet-bearing expected output"
    );
    // init_outlet_registry — outlets handoff (key guard for Task 8/9 refactor)
    assert!(
        src.contains("init_outlet_registry"),
        "missing init_outlet_registry in outlet-bearing expected output (F-1 guard)"
    );
    // setup_outlet_registry — the public entry point
    assert!(
        src.contains("setup_outlet_registry"),
        "missing setup_outlet_registry in outlet-bearing expected output"
    );
    // wasm_runtime — runtime call site
    assert!(
        src.contains("wasm_runtime"),
        "missing wasm_runtime in outlet-bearing expected output"
    );
    // pipeline_pins macro — pins macro output
    assert!(
        src.contains("pipeline_pins"),
        "missing pipeline_pins macro in outlet-bearing expected output"
    );
}
