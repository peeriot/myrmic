#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../common.sh"

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

repo_root="$(pwd -P)"
workspace_parent="$(cd "$repo_root/.." && pwd -P)"
ready="$repo_root/target/hw-logs/mtls-wifi-certs.ready"
config_path="$repo_root/target/hw-logs/mtls-wifi-laptop.swarm.jsonnet"
listen_addr="${MTLS_WIFI_LISTEN_ADDR:-0.0.0.0:7447}"
if [[ "$listen_addr" == tls/* ]]; then
  listen_endpoint="$listen_addr"
else
  listen_endpoint="tls/$listen_addr"
fi

ensure_workspace_root "$repo_root"
mkdir -p "$(dirname "$ready")"
rm -f "$ready"

maybe_generate_certs "$workspace_parent" "false"
touch "$ready"

certs_dir="$workspace_parent/tests/integration/certs"
ca_cert="$(json_str "$(readlink -f "$certs_dir/ca.crt")")"
cert="$(json_str "$(readlink -f "$certs_dir/laptop.crt")")"
key="$(json_str "$(readlink -f "$certs_dir/laptop.key")")"
listen_endpoint="$(json_str "$listen_endpoint")"

cat >"$config_path" <<EOF
local z = import "zenoh.libsonnet";

[
  z.peer()
  + {
    scouting: {
      multicast: { enabled: false },
      gossip: { enabled: false },
    },
    listen: {
      endpoints: [ "$listen_endpoint" ],
    },
    transport: {
      link: {
        tls: {
          enable_mtls: true,
          // ESP32 has a dynamic IP with no DNS name; SNI/hostname verification would
          // always fail. Mutual authentication (client + server cert validation) is
          // still fully enforced via enable_mtls and the CA certificate above.
          verify_name_on_connect: false,
          root_ca_certificate: "$ca_cert",
          listen_certificate: "$cert",
          listen_private_key: "$key",
          connect_certificate: "$cert",
          connect_private_key: "$key",
        },
      },
    },
  }
  + z.plugin("introspection", {})
  + z.plugin("test_control", {}),
]
EOF

swarm_bin="$(select_swarm_bin "$repo_root")"
export RUST_LOG="${RUST_LOG:-info,swarm=info,zenoh=info}"
printf '%s\n' "Swarm mTLS laptop endpoint started with built-in queryables:"
printf '%s\n' "  - @introspection/@v1/@node-status (no payload)"
printf '%s\n' "  - @test/@ctl/@health, @test/@ctl/@stats, @test/@ctl/@introspection (via test_control payloads)"
exec "$swarm_bin" spawn "$config_path"
