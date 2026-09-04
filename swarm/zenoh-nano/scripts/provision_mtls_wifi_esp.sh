#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/common.sh"

usage() {
  cat <<EOF
Usage: $0 --server-addr <ip:port> [options]
Options:
  --wifi-ssid <ssid>
  --wifi-pass <password>
  --esp-port <serial-port>
  --esp-target <target>          (default: riscv32imac-unknown-none-elf)
  --skip-certs
  --esp-release
EOF
}

wifi_ssid=""
wifi_pass=""
server_addr=""
esp_port=""
esp_target="riscv32imac-unknown-none-elf"
skip_certs="false"
esp_release="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h)
      usage
      exit 0
      ;;
    --wifi-ssid)
      [[ $# -ge 2 ]] || fail "--wifi-ssid requires a value"
      wifi_ssid="${2:-}"
      shift 2
      ;;
    --wifi-pass)
      [[ $# -ge 2 ]] || fail "--wifi-pass requires a value"
      wifi_pass="${2:-}"
      shift 2
      ;;
    --server-addr)
      [[ $# -ge 2 ]] || fail "--server-addr requires a value"
      server_addr="${2:-}"
      shift 2
      ;;
    --esp-port)
      [[ $# -ge 2 ]] || fail "--esp-port requires a value"
      esp_port="${2:-}"
      shift 2
      ;;
    --esp-target)
      [[ $# -ge 2 ]] || fail "--esp-target requires a value"
      esp_target="${2:-}"
      shift 2
      ;;
    --skip-certs)
      skip_certs="true"
      shift
      ;;
    --esp-release)
      esp_release="true"
      shift
      ;;
    *)
      fail "Unknown argument: $1"
      ;;
  esac
done

[[ -n "$server_addr" ]] || fail "--server-addr is required (example: 192.168.1.20:7447). Use --help for options."

repo_root="$(pwd -P)"
workspace_parent="$(cd "$repo_root/.." && pwd -P)"

ensure_workspace_root "$repo_root"
maybe_generate_certs "$workspace_parent" "$skip_certs"
build_esp "$workspace_parent" "zenoh_ping_tcp_mtls" "$esp_target" "$esp_release" "$wifi_ssid" "$wifi_pass" "$server_addr"

elf="$(esp_elf_path "$workspace_parent" "$esp_target" "$esp_release" "zenoh_ping_tcp_mtls")"
marker="MTLS_QUERY_NODE_STATUS_V1"
if ! rg --text --quiet "$marker" "$elf"; then
  fail "Built ELF does not contain expected marker '$marker'. Refusing to flash stale firmware."
fi
printf '==> Verified firmware marker: %s\n' "$marker"
flash_and_monitor "$elf" "$esp_port"
