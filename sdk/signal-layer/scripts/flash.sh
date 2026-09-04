#!/usr/bin/env bash
# Flash pre-built ESP32 firmware via espflash.
# Run scripts/build.sh first if the binary does not yet exist.
#
# Usage:
#   scripts/flash.sh [--port <device>] [--target esp32c6] [--monitor]
#
#   --port     — serial port (default: espflash auto-detect)
#   --target   — chip variant (default: esp32c6)
#   --monitor  — open serial monitor after flashing
#
# Note: when running via docker-buildenv.sh, pass the device through with
#   --device /dev/ttyUSB0 or run flash.sh directly on the host.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

usage() {
    echo "Usage: $0 [--port <device>] [--target esp32c6] [--monitor]" >&2
    exit 1
}

PORT=""
TARGET="esp32c6"
MONITOR=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --port)    PORT="$2"; shift 2 ;;
        --target)  TARGET="$2"; shift 2 ;;
        --monitor) MONITOR=1; shift ;;
        *) echo "Unknown argument: $1" >&2; usage ;;
    esac
done

case "$TARGET" in
    esp32c6) BIN_TARGET="riscv32imac-unknown-none-elf" ;;
    *) echo "Unknown target: $TARGET (expected esp32c6)" >&2; exit 1 ;;
esac

FIRMWARE="$REPO_ROOT/target/$BIN_TARGET/release/modem-esp32"

if [[ ! -f "$FIRMWARE" ]]; then
    echo "error: firmware binary not found at $FIRMWARE" >&2
    echo "       run scripts/build.sh first" >&2
    exit 1
fi

FLASH_ARGS=("$FIRMWARE")
[[ -n "$PORT" ]] && FLASH_ARGS+=(--port "$PORT")
[[ "$MONITOR" -eq 1 ]] && FLASH_ARGS+=(--monitor)

echo "==> Flashing firmware ($TARGET)..."
espflash flash "${FLASH_ARGS[@]}"
