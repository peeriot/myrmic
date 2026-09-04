#!/usr/bin/env bash
# Build ESP32 firmware for a given pipeline.
# Runs pipeline codegen first, then compiles the firmware.
#
# Usage:
#   scripts/build.sh <pipeline-name-or-path> [--board <name>] [--target esp32c6]
#
#   pipeline  — bare name (e.g. basic-sensors) or path to a YAML file
#   --board   — board manifest used for codegen (default: esp32c6-devkit)
#   --target  — chip variant for the firmware build (default: esp32c6)
#
# Example:
#   scripts/build.sh basic-sensors
#   scripts/build.sh basic-sensors --target esp32c6

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

usage() {
    echo "Usage: $0 <pipeline-name-or-path> [--board <name>] [--target esp32c6]" >&2
    exit 1
}

[[ $# -lt 1 ]] && usage

PIPELINE="$1"
shift
BOARD="esp32c6-devkit"
TARGET="esp32c6"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --board) BOARD="$2"; shift 2 ;;
        --target) TARGET="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; usage ;;
    esac
done

case "$TARGET" in
    esp32c6) BUILD_ALIAS="build-c6" ;;
    *) echo "Unknown target: $TARGET (expected esp32c6)" >&2; exit 1 ;;
esac

"$SCRIPT_DIR/pipeline_regen.sh" "$PIPELINE" --board "$BOARD" --target "$TARGET"

echo "==> Building firmware for $TARGET..."
(cd "$REPO_ROOT" && cargo +nightly "$BUILD_ALIAS")
echo "Done."
