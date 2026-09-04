#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SWARM_ROOT="$SCRIPT_DIR/../.."
WASM_ROOT="$SWARM_ROOT/../sdk"

echo "Building swarm..."
(cd "$SWARM_ROOT" && cargo build -p swarm-cli)

echo "Building cell-ctl..."
(cd "$SWARM_ROOT" && cargo build -p cell-ctl)

echo "Building cell-tools..."
(cd "$SWARM_ROOT" && cargo build -p cell-tools)

# Make binaries available in the tutorial workspace directory
mkdir -p "$SCRIPT_DIR/workspace"
cp "$SWARM_ROOT/target/debug/swarm" "$SCRIPT_DIR/workspace/swarm"
cp "$SWARM_ROOT/target/debug/cell-ctl" "$SCRIPT_DIR/workspace/cell-ctl"
cp "$SWARM_ROOT/target/debug/cell-tools" "$SCRIPT_DIR/workspace/cell-tools"

echo ""
echo "Setup complete. Binaries placed in workspace/:"
echo "  workspace/swarm"
echo "  workspace/cell-ctl"
echo "  workspace/cell-tools  -- builds cell crates into .wasm binaries and API files"
