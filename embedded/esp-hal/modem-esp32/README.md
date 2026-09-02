# modem-esp32

The flashable firmware binary — the entry point of the embedded stack. Built for one ESP32
SoC, it boots, joins a myrmic swarm over WiFi (and optionally BLE), and hosts a WASM **cell**
on the WAMR runtime. This is the crate you build and flash; the cell it runs is a separate
WASM module from [`../../../sdk/`](../../../sdk/).

Everything it does lives in [`../crates/esp-firmware`](../crates/esp-firmware/) — this crate
is the scaffolding a binary must own, and nothing else:

```rust
#[esp_firmware::main]
async fn setup() {}
```

To build a *different* firmware — your own BLE stack, a native cell instead of a WASM one,
a pin kept back for an LED — you write your own crate against `esp-firmware` rather than
forking this one. See its [README](../crates/esp-firmware/README.md), and
[`../firmware-examples/`](../firmware-examples/) for a crate to copy.

> The **how-to** — building, AOT-compiling a cell, flashing, memory tuning, the flash/partition
> layout, and debugging — lives in the [`esp-hal` README](../README.md). This page is the map
> of the crate itself.

## Source layout

| Path                     | Responsibility                                                                                     |
| ------------------------ | -------------------------------------------------------------------------------------------------- |
| [`src/main.rs`](src/main.rs) | `#![no_std]`/`#![no_main]`, the `#[esp_firmware::main]` entry point, and the Signal Layer pipeline wiring (code-generated into *this* crate, so it cannot live in the library). |
| [`build.rs`](build.rs)   | One call to `esp_firmware_build::configure()`, which generates the flash layout, the app partition table and the main-stack link assert. |
| [`partitions.toml`](partitions.toml.example) | Optional. The firmware/AOT flash split — see the [`esp-hal` README](../README.md). |
| [`nimble-config.toml`](nimble-config.toml) | Compile-time NimBLE configuration, read by `esp-nimble-host`'s build script. |

## Build features

Select exactly one target SoC; the rest are opt-in. The chip feature is declared here rather
than inherited, because `esp_firmware::board!` expands `pins_from_peripherals!`, which reads
the chip from the crate that invokes it.

| Feature                          | Purpose                                                                                                   |
| -------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `esp32c6` (default) / `esp32c5` / `esp32c61` | Target SoC. `esp32c5` and `esp32c61` enable `ble` automatically.                              |
| `ble`                            | NimBLE BLE host stack (user transport). Costs significant RAM — see the [capability matrix](../README.md).|
| `pipeline`                       | Enables the Signal Layer firmware glue. Managed by `esp-codegen` — see [`../signal-layer/`](../signal-layer/); not committed by hand. |
| `report`                         | Emit heap-stats snapshots at key milestones.                                                              |

See the [`esp-hal` README](../README.md) for the supported-chip capability matrix and the full
build/flash/debug workflow.
