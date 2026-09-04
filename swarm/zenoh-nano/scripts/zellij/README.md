## zellij hardware mTLS layouts

Run from the `swarm/` workspace root.

The layouts call dedicated bash pane entry scripts (instead of long inline
shell one-liners):
- `zenoh-nano/scripts/zellij/ble_laptop_pane.sh`
- `zenoh-nano/scripts/zellij/ble_esp_pane.sh`
- `zenoh-nano/scripts/zellij/wifi_laptop_pane.sh`
- `zenoh-nano/scripts/zellij/wifi_esp_pane.sh`

### BLE mTLS (ESP32 <-> Linux zenoh over BLE L2CAP TLS)

Linux runs `zenoh_mtls_pong_ble` (peeriot Zenoh fork + `transport_bt_l2cap_tls`);
ESP32 runs `zenoh_ping_ble_l2cap_mtls` (zenoh-nano + embedded-tls over L2CAP CoC).

No special permissions needed — uses BlueZ via D-Bus, same as any other Bluetooth app.

- pane 1: regenerate certs, run swarm binary listening on `bt_l2cap_tls/<BLE_NAME>`
- pane 2: wait for certs, build/flash ESP BLE Zenoh firmware, serial monitor

```bash
ESP_PORT='/dev/ttyACM0' \
zellij -l zenoh-nano/scripts/zellij/mtls_ble_zenoh_hardware_layout.kdl
```

Optional env vars:

- `BLE_NAME` — BLE name the Linux pong advertises (default: `ZN`)
- `ESP_PORT` — ESP32 serial port (example `/dev/ttyACM0`)
- `ESP_TARGET` — Cargo target (default `riscv32imac-unknown-none-elf`)
- `ESP_RELEASE` — set to `1` for release build

---

### WiFi mTLS (ESP32 <-> Linux zenoh over TLS)

Linux runs `zenoh_mtls_pong` (peeriot Zenoh fork + TLS);
ESP32 runs `zenoh_ping_tcp_mtls` (zenoh-nano + embedded-tls over TCP).

- pane 1: regenerate certs, run `zenoh_mtls_pong`
- pane 2: wait for certs, build/flash ESP WiFi Zenoh firmware, serial monitor

```bash
WIFI_SSID='<ssid>' \
WIFI_PASS='<pass>' \
MTLS_WIFI_SERVER_ADDR='<laptop-lan-ip>:7447' \
MTLS_WIFI_LISTEN_ADDR='0.0.0.0:7447' \
ESP_PORT='/dev/ttyACM0' \
zellij -l zenoh-nano/scripts/zellij/mtls_wifi_hardware_layout.kdl
```

Optional env vars:

- `WIFI_SSID` (required)
- `WIFI_PASS` (required)
- `MTLS_WIFI_SERVER_ADDR` (required — laptop LAN IP:port reachable by ESP)
- `MTLS_WIFI_LISTEN_ADDR` (default `0.0.0.0:7447`)
- `ESP_PORT` (example `/dev/ttyACM0`)
- `ESP_TARGET` (default `riscv32imac-unknown-none-elf`)
