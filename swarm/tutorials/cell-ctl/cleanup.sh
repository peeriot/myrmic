#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "Cleaning up tutorial workspace..."
rm -f "$SCRIPT_DIR/workspace/"*.wasm
rm -f "$SCRIPT_DIR/workspace/cell-ctl"

echo "Cleanup complete."
