#!/usr/bin/env bash
# Regenerate modem-esp32/src/pipeline_config.rs from a board manifest + pipeline
# YAML using the esp-codegen generator.
#
# Usage:
#   scripts/pipeline_regen.sh <pipeline-name-or-path> [--board <name-or-path>]
#
#   pipeline  — bare name (e.g. basic-sensors) resolved under the pipelines
#               directory, or a path to a YAML file
#   --board   — board manifest: bare name (default: esp32c6-devkit) or a path
#
# Examples:
#   scripts/pipeline_regen.sh basic-sensors
#   scripts/pipeline_regen.sh basic-sensors --board esp32c6-devkit
#   scripts/pipeline_regen.sh /path/to/my_pipeline.yaml

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

SIGNAL_MODULES_DIR="$REPO_ROOT/signal-modules"
ESP_SIGNAL_LAYER_DIR="$REPO_ROOT/embedded/esp-hal/signal-layer"
BOARDS_DIR="$ESP_SIGNAL_LAYER_DIR/boards"
PIPELINES_DIR="$ESP_SIGNAL_LAYER_DIR/pipelines"
FIRMWARE_ROOT="$REPO_ROOT/embedded/esp-hal/modem-esp32"

usage() {
    echo "Usage: $0 <pipeline-name-or-path> [--board <name-or-path>]" >&2
    echo "  e.g: $0 basic-sensors" >&2
    echo "  e.g: $0 basic-sensors --board esp32c6-devkit" >&2
    exit 1
}

[[ $# -lt 1 ]] && usage

PIPELINE_ARG="$1"
shift
BOARD_ARG="esp32c6-devkit"
TARGET_ARG="esp32c6"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --board)   BOARD_ARG="$2";   shift 2 ;;
        --target)  TARGET_ARG="$2";  shift 2 ;;
        *) echo "Unknown argument: $1" >&2; usage ;;
    esac
done

# Resolve a bare name (under a directory) or accept an explicit file path.
resolve() {
    local arg="$1" dir="$2"
    if [[ -f "$arg" ]]; then
        echo "$arg"
    else
        echo "$dir/${arg%.yaml}.yaml"
    fi
}

PIPELINE="$(resolve "$PIPELINE_ARG" "$PIPELINES_DIR")"
BOARD="$(resolve "$BOARD_ARG" "$BOARDS_DIR")"
[[ -f "$PIPELINE" ]] || { echo "Pipeline not found: $PIPELINE" >&2; exit 1; }
[[ -f "$BOARD" ]] || { echo "Board manifest not found: $BOARD" >&2; exit 1; }

echo "==> Regenerating pipeline config"
echo "    board:    $BOARD"
echo "    pipeline: $PIPELINE"
(cd "$REPO_ROOT" && cargo +nightly run -p esp-codegen -- \
    --board "$BOARD" \
    --pipeline "$PIPELINE" \
    --drivers "$SIGNAL_MODULES_DIR/drivers" \
    --steps "$SIGNAL_MODULES_DIR/steps" \
    --out "$FIRMWARE_ROOT/src/pipeline_config.rs" \
    --cargo "$FIRMWARE_ROOT/Cargo.toml" \
    --target "$TARGET_ARG")
echo "Done."
