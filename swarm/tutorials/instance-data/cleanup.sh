#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "Stopping myrmic runtimes..."
"$SCRIPT_DIR/workspace/myrmic" runtimes delete default 2>/dev/null || true
sleep 1

echo "Cleaning up workspace contents..."
find "$SCRIPT_DIR/workspace" -mindepth 1 -maxdepth 1 \
    ! -name '.gitignore' \
    -exec rm -rf {} +

echo "Cleanup complete."
