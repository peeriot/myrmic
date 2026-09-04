#!/usr/bin/env bash
# Run an esp-hal script inside the edgevance-node-buildenv Docker container.
#
# Usage:
#   scripts/docker-buildenv.sh <script> [args...]
#
#   <script> — bare script name (looked up in scripts/) or a path relative
#              to the repo root
#
# Examples:
#   scripts/docker-buildenv.sh pipeline_regen.sh basic-sensors
#   scripts/docker-buildenv.sh build.sh basic-sensors --target esp32c6
#
# For flashing, the USB device must be passed through explicitly:
#   docker run --device /dev/ttyUSB0 ... (or run flash.sh directly on the host)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
IMAGE="ghcr.io/peeriot/edgevance-node-buildenv:latest"

usage() {
    echo "Usage: $0 <script> [args...]" >&2
    echo "  e.g: $0 pipeline_regen.sh basic-sensors" >&2
    echo "  e.g: $0 build.sh basic-sensors --target esp32c6" >&2
    exit 1
}

[[ $# -lt 1 ]] && usage

SCRIPT_ARG="$1"
shift

# Bare name → look up in the scripts directory inside the container
if [[ "$SCRIPT_ARG" != */* ]]; then
    SCRIPT_IN_CONTAINER="/work/sdk/signal-layer/scripts/$SCRIPT_ARG"
else
    # Path relative to repo root (strip leading ./ if present)
    SCRIPT_ARG="${SCRIPT_ARG#./}"
    SCRIPT_IN_CONTAINER="/work/$SCRIPT_ARG"
fi

DOCKER_ARGS=(
    --rm
    --volume "$REPO_ROOT:/work"
    --volume "edgevance-node-cargo-cache:/cache/cargo"
    --workdir /work
)

if [[ -n "${SSH_AUTH_SOCK:-}" ]]; then
    DOCKER_ARGS+=(--volume "$SSH_AUTH_SOCK:/ssh-agent" --env SSH_AUTH_SOCK=/ssh-agent)
fi

exec docker run "${DOCKER_ARGS[@]}" "$IMAGE" "$SCRIPT_IN_CONTAINER" "$@"
