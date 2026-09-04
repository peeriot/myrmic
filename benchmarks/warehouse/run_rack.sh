#!/usr/bin/env bash
# Runs the warehouse benchmark against a rack of real hosts over SSH (see
# `warehouse-rack-full.toml`/`warehouse-rack-smoke.toml`), handling the preconditions a plain
# `cargo run --config ...` leaves for the caller (building a native `myrmic` for cell builds,
# cross-compiling and uploading a separate `myrmic` to every host) and the postcondition (tearing
# every host's runtime back down) that must run even if the benchmark itself fails.
#
# Usage: benchmarks/warehouse/run_rack.sh <config.toml> [load...]
#   defaults to the standard sweep if no loads are given.
#
# Requires SSH access to every host named in <config.toml>'s [specialized.rack] table, and the
# `gcc-aarch64-linux-gnu` package installed locally (rustup target already added via
# rust-toolchain.toml; the cross gcc/linker itself is not).

set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 <config.toml> [load...]" >&2
  exit 1
fi
CONFIG="$1"
shift

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RACK_TARGET="aarch64-unknown-linux-gnu"
OUTPUT_DIR="$SCRIPT_DIR/warehouse-rack-report"

LOADS=("$@")
if [ ${#LOADS[@]} -eq 0 ]; then
  LOADS=(200 400 600 800 1000 1250 1500 1750 2000)
fi

echo "building warehouse-bench, rack-ctl, and bench-report's merge binary for this machine..."
cargo build --release -p warehouse-benchmark --manifest-path "$REPO_ROOT/Cargo.toml"
cargo build --release -p bench-report --bin merge --manifest-path "$REPO_ROOT/Cargo.toml"

# Separate from the aarch64 cross-compile below: `Myrmic::local()` (see
# `test_framework::myrmic::Myrmic::local`) needs a *native* `myrmic` on this machine to build the
# wasm cells and register them over zenoh through the tunnel — resolved via `resolve_binary!`,
# i.e. `target/release/myrmic`, which nothing else in this script produces.
echo "building myrmic for this machine..."
cargo build --release -p myrmic-cli --manifest-path "$REPO_ROOT/Cargo.toml"

echo "cross-compiling myrmic for the rack ($RACK_TARGET)..."
# The repo-wide `~/.cargo/config.toml` convention on this kind of dev box sets `-fuse-ld=mold`
# for every `cfg(target_os = "linux")` target, but the plain `cc` cargo otherwise picks as the
# aarch64 linker is the host's native (x86_64) gcc frontend — it doesn't know to target aarch64,
# so it hands mold arm64 object files while set up to link x86_64, and mold rejects them
# ("incompatible file type: x86_64 is expected but got arm64"). Point cargo at the actual aarch64
# cross gcc (from the `gcc-aarch64-linux-gnu` package) instead, which sets up the target
# correctly before mold ever runs.
# `dist` (not `release`): same optimizations, but stripped of debug symbols — this binary only
# ever gets uploaded and run, never locally debugged, and stripping cuts the (slow, uplink-bound)
# per-host upload size roughly in third.
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
  cargo build --profile dist -p myrmic-cli --target "$RACK_TARGET" --manifest-path "$REPO_ROOT/Cargo.toml"

WAREHOUSE_BENCH="$REPO_ROOT/target/release/warehouse-bench"
RACK_CTL="$REPO_ROOT/target/release/rack-ctl"
MERGE="$REPO_ROOT/target/release/merge"
MYRMIC_BIN="$REPO_ROOT/target/$RACK_TARGET/dist/myrmic"

cleanup() {
  echo "=== tearing down rack hosts ==="
  "$RACK_CTL" cleanup --config "$CONFIG" || echo "warning: rack cleanup failed; hosts may need manual teardown"
}
trap cleanup EXIT

echo "=== provisioning rack hosts ==="
"$RACK_CTL" upload --config "$CONFIG" --binary "$MYRMIC_BIN"

# Start clean: a stale per-load JSON left over from an earlier, possibly different-version run
# must not silently end up in this sweep's merge.
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

JSON_FILES=()
for load in "${LOADS[@]}"; do
  echo "=== running load ${load}/sec ==="
  "$WAREHOUSE_BENCH" --config "$CONFIG" --load "$load" --output-dir "$OUTPUT_DIR"
  JSON_FILES+=("$OUTPUT_DIR/$load.json")
done

echo "merging ${#JSON_FILES[@]} load(s) into $OUTPUT_DIR..."
"$MERGE" --output-dir "$OUTPUT_DIR" "${JSON_FILES[@]}"
