#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SWARM_ROOT="$SCRIPT_DIR/../.."
WASM_ROOT="$SWARM_ROOT/../sdk"

echo "Building swarm..."
(cd "$SWARM_ROOT" && cargo build -p swarm-cli)

echo "Building myrmic-cli..."
(cd "$SWARM_ROOT" && cargo build -p myrmic-cli)

# Copy binaries into workspace/ so the tutorial commands
# (and swarm-config.jsonnet) can use ./workspace/<name> consistently.
mkdir -p "$SCRIPT_DIR/workspace"
cp "$SWARM_ROOT/target/debug/swarm"                                     "$SCRIPT_DIR/workspace/swarm"
cp "$SWARM_ROOT/target/debug/myrmic"                                    "$SCRIPT_DIR/workspace/myrmic"

echo ""
echo "Setup complete. Files placed in workspace/:"
echo "  workspace/swarm             -- the swarm runtime"
echo "  workspace/myrmic            -- the myrmic CLI (scaffold, build, deploy, manage)"
