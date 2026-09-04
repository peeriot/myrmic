#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../common.sh"

repo_root="$(pwd -P)"
ready="$repo_root/target/hw-logs/mtls-ble-zenoh-certs.ready"
esp_target="${ESP_TARGET:-riscv32imac-unknown-none-elf}"
esp_port="${ESP_PORT:-}"
esp_release="${ESP_RELEASE:-0}"
script="$repo_root/zenoh-nano/scripts/provision_mtls_ble_zenoh_esp.sh"

ensure_workspace_root "$repo_root"
wait_for_ready "$ready"

args=(
  --skip-certs
  --esp-target "$esp_target"
)
[[ "$esp_release" == "1" ]] && args+=(--esp-release)
[[ -n "$esp_port" ]] && args+=(--esp-port "$esp_port")

exec bash "$script" "${args[@]}"
