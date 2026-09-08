#!/usr/bin/env bash
# ci/check-generated-pipeline.sh
#
# Proves the linux-codegen chain produces a compilable, working crate.
#
# Steps:
#   1. Run linux-codegen on the rpi-basic reference example.
#   2. cargo check the generated standalone crate.
#   3. cargo clippy -- -D warnings on the generated crate.
#   4. cargo test on the generated crate (runs tap_contract.rs).
#   5. Confirm critical-section/std resolves in the generated crate on its own.
#      It declares its own [workspace], so the restore-state-bool vs u32
#      collision the host workspace has cannot reach it.
#
# CI-runnable without hardware: the generated crate compiles without /dev/i2c-*
# (LinuxI2cdev::new is only called at runtime). The tap_contract.rs test
# self-hosts run_tap_server on a tempdir UDS in-process and asserts the
# retained/event/drain-batch contracts fully - no hardware, no skip path.
#
# Usage (from repo root):
#   ./ci/check-generated-pipeline.sh
#
# Exit codes:
#   0 - all steps passed
#   1 - one or more steps failed (see output for details)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PIPELINE_YAML="$REPO_ROOT/examples/rpi-basic/pipeline.yaml"
MANIFEST_YAML="$REPO_ROOT/examples/rpi-basic/raspberry-pi.yaml"
DRIVERS_DIR="$REPO_ROOT/signal-modules/drivers"
STEPS_DIR="$REPO_ROOT/signal-modules/steps"

# Generate into a temp dir INSIDE sdk/signal-layer/ so the relative paths
# in the generated Cargo.toml resolve correctly (they navigate up with ../).
TMPDIR_BASE="$(mktemp -d "$REPO_ROOT/sdk/signal-layer/generated-pipeline-XXXXXX")"
trap 'rm -rf "$TMPDIR_BASE"' EXIT

echo "=== Step 1: linux-codegen ==="
echo "  pipeline : $PIPELINE_YAML"
echo "  manifest : $MANIFEST_YAML"
echo "  out      : $TMPDIR_BASE"

(
    cd "$REPO_ROOT"
    cargo run -p linux-codegen -- \
        --pipeline "$PIPELINE_YAML" \
        --manifest "$MANIFEST_YAML" \
        --drivers "$DRIVERS_DIR" \
        --steps "$STEPS_DIR" \
        --out "$TMPDIR_BASE"
)
echo "  [PASS] linux-codegen generated crate"

echo ""
echo "=== Step 2: cargo check ==="
(
    cd "$TMPDIR_BASE"
    cargo check 2>&1
)
echo "  [PASS] cargo check"

echo ""
echo "=== Step 3: cargo clippy ==="
(
    cd "$TMPDIR_BASE"
    cargo clippy -- -D warnings 2>&1
)
echo "  [PASS] cargo clippy -D warnings"

echo ""
echo "=== Step 4: cargo test (tap_contract.rs) ==="
(
    cd "$TMPDIR_BASE"
    cargo test 2>&1
)
echo "  [PASS] cargo test"

echo ""
echo "=== Step 5: critical-section/std resolves in the standalone crate ==="
# The generated crate has its own [workspace] and critical-section = { features = ["std"] }.
# Step 2 already compiled it, so this step only reports why that is the whole
# check: from the repo root, `cargo test --workspace --no-run` hits a
# restore-state-bool vs restore-state-u32 collision (esp-hal forces u32; several
# host-target crates force bool via critical-section/std). The generated crate
# never sees esp-hal, so the collision cannot reach it.
echo "  [INFO] Generated Cargo.toml declares critical-section/std:"
grep "critical-section" "$TMPDIR_BASE/Cargo.toml" || true
echo "  [INFO] The generated crate compiled in Step 2 - no esp-hal in its workspace."
echo "  [PASS] critical-section/std resolves (standalone crate is outside the workspace)"

echo ""
echo "=== Step 6: actuator pipeline (cell-driven + feed-forward outlets) ==="
# Same chain for the mixed outlet fixture: proves the actuator emission paths
# (linux-gpio-shim pins/PWM, inline feed-forward applies, cell-driven sink task
# + IPC outlet store) produce a crate that compiles and lints, and runs the
# generated tap/outlet contract tests. Uses the linux-codegen test fixtures as
# the reference input. Compiles without /dev/gpiochip* or /sys/class/pwm -
# pins are only opened at runtime.
ACTUATORS_FIXTURES="$REPO_ROOT/sdk/signal-layer/linux-codegen/tests/fixtures"
TMPDIR_ACTUATORS="$(mktemp -d "$REPO_ROOT/sdk/signal-layer/generated-actuators-XXXXXX")"
trap 'rm -rf "$TMPDIR_BASE" "$TMPDIR_ACTUATORS"' EXIT

(
    cd "$REPO_ROOT"
    cargo run -p linux-codegen -- \
        --pipeline "$ACTUATORS_FIXTURES/linux-outlets.yaml" \
        --manifest "$ACTUATORS_FIXTURES/linux-actuators-manifest.yaml" \
        --drivers "$DRIVERS_DIR" \
        --steps "$STEPS_DIR" \
        --out "$TMPDIR_ACTUATORS"
)
(
    cd "$TMPDIR_ACTUATORS"
    cargo check 2>&1
    cargo clippy -- -D warnings 2>&1
    cargo test 2>&1
)
echo "  [PASS] actuator pipeline generated, checked, linted, tested"

echo ""
echo "=== Step 7: SPI loopback pipeline (spidev + software CS) ==="
# Same chain for the SPI fixture: proves the spidev/software-CS emission path
# (linux-spi-shim bus + per-device CS lines) produces a crate that compiles
# and lints. Compiles without /dev/spidev* - nodes are only opened at runtime.
TMPDIR_SPI="$(mktemp -d "$REPO_ROOT/sdk/signal-layer/generated-spi-XXXXXX")"
trap 'rm -rf "$TMPDIR_BASE" "$TMPDIR_ACTUATORS" "$TMPDIR_SPI"' EXIT

(
    cd "$REPO_ROOT"
    cargo run -p linux-codegen -- \
        --pipeline "$ACTUATORS_FIXTURES/linux-spi.yaml" \
        --manifest "$ACTUATORS_FIXTURES/linux-spi-manifest.yaml" \
        --drivers "$DRIVERS_DIR" \
        --steps "$STEPS_DIR" \
        --out "$TMPDIR_SPI"
)
(
    cd "$TMPDIR_SPI"
    cargo check 2>&1
    cargo clippy -- -D warnings 2>&1
    cargo test 2>&1
)
echo "  [PASS] SPI pipeline generated, checked, linted, tested"

echo ""
echo "=== All steps PASSED ==="
