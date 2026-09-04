#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../common.sh"

repo_root="$(pwd -P)"
workspace_parent="$(cd "$repo_root/.." && pwd -P)"
ready="$repo_root/target/hw-logs/mtls-ble-zenoh-certs.ready"
config_path="$repo_root/target/hw-logs/mtls-ble-laptop.swarm.jsonnet"
ble_name="${BLE_NAME:-ZN}"
listen_endpoint="bt_l2cap_tls/$ble_name"

ensure_workspace_root "$repo_root"
mkdir -p "$(dirname "$ready")"
rm -f "$ready"

maybe_generate_certs "$workspace_parent" "false"
touch "$ready"

certs_dir="$workspace_parent/tests/integration/certs"
ca_cert="$(json_str "$(readlink -f "$certs_dir/ca.crt")")"
cert="$(json_str "$(readlink -f "$certs_dir/laptop.crt")")"
key="$(json_str "$(readlink -f "$certs_dir/laptop.key")")"
listen_endpoint_json="$(json_str "$listen_endpoint")"

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
      endpoints: [ "$listen_endpoint_json" ],
    },
    transport: {
      link: {
        tls: {
          enable_mtls: true,
          // ESP32 has no DNS name; SNI/hostname verification is not applicable over BLE.
          // Mutual authentication via CA certificate is still fully enforced.
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
printf '%s\n' "Swarm BLE mTLS laptop endpoint ($listen_endpoint) started with built-in queryables:"
printf '%s\n' "  - @introspection/@v1/@node-status (no payload)"
printf '%s\n' "  - @test/@ctl/@health, @test/@ctl/@stats, @test/@ctl/@introspection (via test_control payloads)"
exec "$swarm_bin" spawn "$config_path"
