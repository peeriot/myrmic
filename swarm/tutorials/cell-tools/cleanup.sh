#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "Stopping any background swarm process..."
pkill -f "swarm.*swarm-config" 2>/dev/null || true
sleep 1

echo "Cleaning up workspace contents..."
find "$SCRIPT_DIR/workspace" -mindepth 1 -maxdepth 1 \
    ! -name '.gitignore' \
    -exec rm -rf {} +

echo "Cleanup complete."
