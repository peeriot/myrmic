# modem-esp32

The flashable firmware binary — the entry point of the embedded stack. Built for one ESP32
SoC, it boots, joins a myrmic swarm over WiFi (and optionally BLE), and hosts a WASM **cell**
on the WAMR runtime. This is the crate you build and flash; the cell it runs is a separate
WASM module from [`../../../sdk/`](../../../sdk/).

> The **how-to** — building, AOT-compiling a cell, flashing, memory tuning, the flash/partition
> layout, and debugging — lives in the [`esp-hal` README](../README.md). This page is the map
> of the crate itself.

## Source layout

| Path                     | Responsibility                                                                                     |
| ------------------------ | -------------------------------------------------------------------------------------------------- |
| [`src/main.rs`](src/main.rs) | Firmware entry point: heap and executor setup, then brings up the network service and the WASM host. |
| [`src/wasm.rs`](src/wasm.rs) | Task wrappers around the WASM module lifecycle (the `wasm-runtime` `service` module: transfer, flash load, WAMR thread). |
| [`src/network.rs`](src/network.rs) | Task wrappers around the network bring-up ([`../crates/esp-network`](../crates/esp-network): WiFi, embassy-net, zenoh session) and the session-scoped services ([`../crates/cell-db-service`](../crates/cell-db-service)). |
| [`src/stack_hwm.rs`](src/stack_hwm.rs) | Optional main-stack high-water-mark measurement (`stack-hwm` feature).                  |

Liveness supervision, the hardware watchdog and the heap-stats reporter live in
[`../crates/esp-watchdog`](../crates/esp-watchdog); the binary only wraps the supervisor in a
task and forwards the `hang-record` / `report` / `wdt-characterize` features.

## Build features

Select exactly one target SoC; the rest are opt-in.

| Feature                          | Purpose                                                                                                   |
| -------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `esp32c6` (default) / `esp32c5` / `esp32c61` | Target SoC. `esp32c5` and `esp32c61` enable `ble` automatically.                              |
| `ble`                            | NimBLE BLE host stack (user transport). Costs significant RAM — see the [capability matrix](../README.md).|
| `pipeline`                       | Enables the Signal Layer firmware glue. Managed by `esp-codegen` — see [`../signal-layer/`](../signal-layer/); not committed by hand. |
| `report`                         | Emit heap-stats snapshots at key milestones.                                                              |

See the [`esp-hal` README](../README.md) for the supported-chip capability matrix and the full
build/flash/debug workflow.
