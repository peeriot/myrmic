#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../common.sh"

wait_for_ready() {
  local ready="$1"
  local attempts=0
  until [[ -f "$ready" ]]; do
    attempts=$(( attempts + 1 ))
    if (( attempts >= 300 )); then
      fail "timed out after 60s waiting for laptop pane to write $ready"
    fi
    sleep 0.2
  done
}

repo_root="$(pwd -P)"
ready="$repo_root/target/hw-logs/mtls-wifi-certs.ready"
wifi_ssid="$(must_env WIFI_SSID)"
wifi_pass="$(must_env WIFI_PASS)"
server_addr="$(must_env MTLS_WIFI_SERVER_ADDR)"
esp_target="${ESP_TARGET:-riscv32imac-unknown-none-elf}"
esp_port="${ESP_PORT:-}"
script="$repo_root/zenoh-nano/scripts/provision_mtls_wifi_esp.sh"

ensure_workspace_root "$repo_root"
wait_for_ready "$ready"

args=(
  --skip-certs
  --esp-release
  --wifi-ssid "$wifi_ssid"
  --wifi-pass "$wifi_pass"
  --server-addr "$server_addr"
  --esp-target "$esp_target"
)
if [[ -n "$esp_port" ]]; then
  args+=(--esp-port "$esp_port")
fi

exec bash "$script" "${args[@]}"
