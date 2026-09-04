#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SWARM_ROOT="$SCRIPT_DIR/../.."
WASM_ROOT="$SWARM_ROOT/../sdk"
WASM_TARGET="$WASM_ROOT/target/wasm32-unknown-unknown/release"

echo "Building swarm..."
(cd "$SWARM_ROOT" && cargo build -p swarm-cli)

echo "Building cell-ctl..."
(cd "$SWARM_ROOT/cell-ctl" && cargo build)

echo "Building room cell..."
(cd "$WASM_ROOT/module-examples/cell-room-wasm" && cargo build --release)

echo "Building thermostat cell..."
(cd "$WASM_ROOT/module-examples/cell-thermostat-wasm" && cargo build --release)

echo "Moving binaries to tutorial workspace..."
mv "$WASM_TARGET/room.wasm" "$SCRIPT_DIR/workspace/room.wasm"
mv "$WASM_TARGET/thermostat.wasm" "$SCRIPT_DIR/workspace/thermostat.wasm"
cp "$SWARM_ROOT/target/debug/cell-ctl" "$SCRIPT_DIR/workspace/cell-ctl"

echo "Setup complete."
