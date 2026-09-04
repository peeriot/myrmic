#!/usr/bin/env bash
set -euo pipefail

fail() {
  local msg="${1:-unknown error}"
  local code="${2:-1}"
  printf '[FAIL] %s\n' "$msg" >&2
  exit "$code"
}

ensure_workspace_root() {
  local repo_root="$1"
  [[ -f "$repo_root/Cargo.toml" ]] || fail "Run from the swarm/ workspace root (directory containing Cargo.toml)."
}

openssl_ok() {
  openssl "$@" >/dev/null 2>&1
}

certs_valid() {
  local workspace_parent="$1"
  local certs_dir="$workspace_parent/tests/integration/certs"

  local required=(
    "$certs_dir/ca.crt"
    "$certs_dir/ca.der"
    "$certs_dir/laptop.crt"
    "$certs_dir/laptop.der"
    "$certs_dir/laptop.key"
    "$certs_dir/laptop.key.der"
    "$certs_dir/laptop.key.pkcs8.der"
    "$certs_dir/esp.crt"
    "$certs_dir/esp.der"
    "$certs_dir/esp.key"
    "$certs_dir/esp.key.der"
    "$certs_dir/esp.key.pkcs8.der"
  )

  local p
  for p in "${required[@]}"; do
    [[ -f "$p" ]] || return 1
  done

  local ca="$certs_dir/ca.crt"
  local laptop_crt="$certs_dir/laptop.crt"
  local esp_crt="$certs_dir/esp.crt"

  openssl_ok verify --CAfile "$ca" "$laptop_crt" || return 1
  openssl_ok verify --CAfile "$ca" "$esp_crt" || return 1
  openssl_ok x509 -in "$ca" -noout -checkend 0 || return 1
  openssl_ok x509 -in "$laptop_crt" -noout -checkend 0 || return 1
  openssl_ok x509 -in "$esp_crt" -noout -checkend 0 || return 1

  return 0
}

maybe_generate_certs() {
  local workspace_parent="$1"
  local skip_certs="$2"
  local certs_dir="$workspace_parent/tests/integration/certs"
  local cert_script="$workspace_parent/tests/integration/gen-test-certs.sh"

  [[ "$skip_certs" == "true" ]] && return 0
  [[ -f "$cert_script" ]] || fail "Certificate generator not found: $cert_script"

  if certs_valid "$workspace_parent"; then
    printf '%s\n' "==> Reusing existing test certificates (valid)"
    return 0
  fi

  printf '%s\n' "==> Generating test certificates"
  bash "$cert_script" --certs-dir "$certs_dir"
  certs_valid "$workspace_parent" || fail "Generated certificates are not valid."
}

esp_elf_path() {
  local workspace_parent="$1"
  local esp_target="$2"
  local esp_release="$3"
  local bin_name="$4"
  local profile="debug"
  [[ "$esp_release" == "true" ]] && profile="release"

  local workspace_elf="$workspace_parent/swarm/target/$esp_target/$profile/$bin_name"
  local package_elf="$workspace_parent/embedded/examples/esp/target/$esp_target/$profile/$bin_name"

  if [[ -f "$workspace_elf" ]]; then
    printf '%s\n' "$workspace_elf"
  else
    printf '%s\n' "$package_elf"
  fi
}

build_esp() {
  local workspace_parent="$1"
  local bin_name="$2"
  local esp_target="$3"
  local esp_release="$4"
  local wifi_ssid="$5"
  local wifi_pass="$6"
  local server_addr="$7"

  local manifest="$workspace_parent/embedded/examples/esp/Cargo.toml"
  [[ -f "$manifest" ]] || fail "ESP manifest not found: $manifest"

  local args=(+nightly build --manifest-path "$manifest" --bin "$bin_name" --target "$esp_target")
  [[ "$esp_release" == "true" ]] && args+=(--release)

  printf '==> Building %s\n' "$bin_name"
  # NOTE: WIFI_PASS is visible to build scripts via env::vars() — acceptable for a
  # test harness. Do not add set -x above this line or the password will leak into logs.
  WIFI_SSID="$wifi_ssid" WIFI_PASS="$wifi_pass" MTLS_WIFI_SERVER_ADDR="$server_addr" cargo "${args[@]}"

  local elf
  elf="$(esp_elf_path "$workspace_parent" "$esp_target" "$esp_release" "$bin_name")"
  [[ -f "$elf" ]] || fail "ELF not found after build: $elf"
}

flash_and_monitor() {
  local elf="$1"
  local esp_port="$2"
  local args=(flash --monitor --non-interactive)
  [[ -n "$esp_port" ]] && args+=(--port "$esp_port")
  args+=("$elf")
  printf '%s\n' "==> Flashing and monitoring"
  espflash "${args[@]}"
}

must_env() {
  local name="$1"
  local value="${!name:-}"
  [[ -n "$value" ]] || fail "environment variable $name is required but not set"
  printf '%s\n' "$value"
}

json_str() {
  # Escape a value for safe embedding inside a JSON/Jsonnet double-quoted string.
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

flash_esp() {
  local elf="$1"
  local esp_port="$2"
  local args=(flash)
  [[ -n "$esp_port" ]] && args+=(--port "$esp_port")
  args+=("$elf")
  printf '%s\n' "==> Flashing ESP firmware"
  espflash "${args[@]}"
}

select_swarm_bin() {
  local repo_root="$1"
  local release="$repo_root/target/release/swarm"
  local debug="$repo_root/target/debug/swarm"
  if [[ -x "$release" ]]; then
    printf '%s\n' "$release"
    return 0
  fi
  if [[ -x "$debug" ]]; then
    printf '%s\n' "$debug"
    return 0
  fi
  fail "swarm binary not found at target/release/swarm or target/debug/swarm; run cargo build -p swarm"
}

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
