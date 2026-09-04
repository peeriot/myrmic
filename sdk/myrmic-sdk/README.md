# Myrmic SDK

The Rust SDK for writing Myrmic cells: Wasm modules deployed and managed by the
sorg infrastructure.

It provides the host function bindings a cell calls into (`db`, `tap`, `outlet`,
`gpio`, `ble`, `gateway`, cell messaging, logging, time) and, via
[`myrmic-sdk-macros`](https://crates.io/crates/myrmic-sdk-macros), the
`#[cmd]`/`#[monitor]`/`#[init]` macros that export a cell's functions to the
host.
