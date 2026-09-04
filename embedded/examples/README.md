# Examples

Runnable example firmware binaries for the ESP32 target. Each `.rs` under
[`esp/src/bin/`](esp/src/bin/) is a standalone flashable binary demonstrating one capability
of the embedded stack — onboarding a host into a swarm, and Zenoh peer-to-peer ping — over
both BLE and TCP transports.

## Binaries

| Binary                                | Demonstrates                                                        |
| ------------------------------------- | ------------------------------------------------------------------- |
| `swarm_onboarding_tcp`                | Onboarding a host over TCP.                                         |
| `swarm_onboarding_ble`                | Onboarding a host over BLE.                                         |
| `swarm_onboarding_ble_listener`       | The BLE listener side of onboarding.                                |
| `zenoh_ping_tcp` / `zenoh_ping_tcp_mtls` | Zenoh ping over TCP (plain and mTLS).                            |
| `zenoh_ping_ble` / `zenoh_ping_ble_listener` | Zenoh ping over BLE.                                        |
| `zenoh_ping_ble_l2cap_mtls`           | Zenoh ping over BLE L2CAP with mTLS.                                |

The `mtls` variants use the bundled onboarding certificates in [`esp/src/bin/`](esp/src/bin/).
See [`../esp-hal/README.md`](../esp-hal/README.md) for the supported chips and build/flash
setup.
