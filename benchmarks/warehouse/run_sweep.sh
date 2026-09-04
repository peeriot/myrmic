#!/usr/bin/env bash
# Runs the warehouse benchmark once per load, each against its own fresh swarm process, then
# merges the resulting per-load JSON files into one report (report.json + report.pdf).
#
# A fresh process per load is what keeps the loads' numbers independent of each other — sharing
# one long-lived swarm across loads let a backlog straggling past one pass's `drain_timeout` keep
# draining into whichever pass was current when it finally caught up, silently crediting (or
# blaming) the wrong load for it.
#
# Usage: benchmarks/warehouse/run_sweep.sh [load...]
#   defaults to the standard sweep if no loads are given.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CONFIG="$SCRIPT_DIR/warehouse.toml"
OUTPUT_DIR="$SCRIPT_DIR/warehouse-report"

LOADS=("$@")
if [ ${#LOADS[@]} -eq 0 ]; then
  LOADS=(200 400 600 800 1000 1250 1500 1750 2000)
fi

echo "building warehouse-bench, bench-report's merge binary, myrmic (warehouse-bench shells out to it for cell deploys), and the swarm binary each pass spawns..."
cargo build --release -p warehouse-benchmark --manifest-path "$REPO_ROOT/Cargo.toml"
cargo build --release -p bench-report --bin merge --manifest-path "$REPO_ROOT/Cargo.toml"
cargo build --release -p myrmic-cli --manifest-path "$REPO_ROOT/Cargo.toml"
# telemetry-export-db is what makes the spawned swarm process write command/event counters to the
# DB tables `cell_interaction_metrics` reads — without it, hop coverage silently reports 0% for
# every hop on every pass, even though the cells are actually processing everything.
cargo build --release -p swarm-cli --bin swarm --features telemetry-export-db --manifest-path "$REPO_ROOT/Cargo.toml"

WAREHOUSE_BENCH="$REPO_ROOT/target/release/warehouse-bench"
MERGE="$REPO_ROOT/target/release/merge"

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
