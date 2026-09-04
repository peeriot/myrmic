#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SWARM_ROOT="$SCRIPT_DIR/../../.."

echo "Building myrmic-cli..."
(cd "$SWARM_ROOT" && cargo build -p myrmic-cli)

mkdir -p "$SCRIPT_DIR/workspace"
cp "$SWARM_ROOT/target/debug/myrmic" "$SCRIPT_DIR/workspace/myrmic"

echo ""
echo "Setup complete. Files placed in workspace/:"
echo "  workspace/myrmic   -- the myrmic CLI (scaffold, build, deploy, manage, runtime)"
