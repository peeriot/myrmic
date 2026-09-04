# Crates

The low-level building blocks the [`modem-esp32`](../modem-esp32/) firmware is assembled from.
These are the chip-facing pieces — the WASM runtime integration and the flash/MMU plumbing
that lets WAMR run AOT modules execute-in-place from flash. Application logic does not live
here; it lives in the WASM cells under [`../../../sdk/`](../../../sdk/).

| Crate                                         | What it does                                                                                              |
| --------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| [`wasm-runtime`](wasm-runtime/)               | The WAMR integration that actually executes WASM modules, plus the host imports a cell calls.            |
| [`wasm-runtime-macros`](wasm-runtime-macros/) | Proc-macro helpers for `wasm-runtime`.                                                                   |
| [`wasm-storage`](wasm-storage/)               | Stores and loads the AOT WASM module in its dedicated flash region.                                      |
| [`esp-mmu`](esp-mmu/)                         | MMU driver: maps/unmaps virtual CPU addresses to physical SPI flash — the basis for AOT execute-in-place.|
| [`esp-mmu-consts`](esp-mmu-consts/)           | Dependency-free per-chip MMU hardware constants, split out so host tooling can share them.               |

For how these fit together — AOT XIP operation, the flash/partition layout, and memory
tuning — see the [`esp-hal` README](../README.md).
