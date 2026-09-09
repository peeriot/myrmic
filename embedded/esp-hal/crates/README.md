# Crates

The building blocks a myrmic firmware is assembled from. [`esp-firmware`](esp-firmware/) is
the assembly itself — the library [`modem-esp32`](../modem-esp32/) and any custom firmware are
built on; the rest are the pieces it drives.
These are the chip-facing pieces — the WASM runtime integration and the flash/MMU plumbing
that lets WAMR run AOT modules execute-in-place from flash. Application logic does not live
here; it lives in the WASM cells under [`../../../sdk/`](../../../sdk/).

| Crate                                         | What it does                                                                                              |
| --------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| [`esp-firmware`](esp-firmware/)               | **The firmware, as a library.** Boots the node and hosts a cell; hardware you take off its `Board` is hardware it does not drive. |
| [`esp-firmware-macros`](esp-firmware-macros/) | The `#[esp_firmware::main]` entry-point attribute.                                                       |
| [`esp-firmware-build`](esp-firmware-build/)   | Build-script support: flash partition layout, app partition table, main-stack link assert.               |
| [`wasm-runtime`](wasm-runtime/)               | The WAMR integration that actually executes WASM modules, plus the host imports a cell calls.            |
| [`wasm-runtime-macros`](wasm-runtime-macros/) | Proc-macro helpers for `wasm-runtime`.                                                                   |
| [`wasm-storage`](wasm-storage/)               | Stores and loads the AOT WASM module in its dedicated flash region.                                      |
| [`esp-mmu`](esp-mmu/)                         | MMU driver: maps/unmaps virtual CPU addresses to physical SPI flash — the basis for AOT execute-in-place.|
| [`esp-mmu-consts`](esp-mmu-consts/)           | Dependency-free per-chip MMU hardware constants, split out so host tooling can share them.               |

For how these fit together — AOT XIP operation, the flash/partition layout, and memory
tuning — see the [`esp-hal` README](../README.md).
