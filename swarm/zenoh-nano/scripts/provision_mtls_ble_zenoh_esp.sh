#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/common.sh"

usage() {
  cat <<EOF
Usage: $0 [options]
Options:
  --esp-port <serial-port>
  --esp-target <target>          (default: riscv32imac-unknown-none-elf)
  --skip-certs
  --esp-release
EOF
}

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

repo_root="$(pwd -P)"
workspace_parent="$(cd "$repo_root/.." && pwd -P)"

ensure_workspace_root "$repo_root"
maybe_generate_certs "$workspace_parent" "$skip_certs"
build_esp "$workspace_parent" "zenoh_ping_ble_l2cap_mtls" "$esp_target" "$esp_release" "" "" ""

elf="$(esp_elf_path "$workspace_parent" "$esp_target" "$esp_release" "zenoh_ping_ble_l2cap_mtls")"
marker="@introspection/@v1/@node-status"
if ! rg --text --quiet --fixed-strings "$marker" "$elf"; then
  fail "Built ELF does not contain expected marker '$marker'. Refusing to flash stale firmware."
fi
printf '==> Verified firmware marker: %s\n' "$marker"
flash_and_monitor "$elf" "$esp_port"
