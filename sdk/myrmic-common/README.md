# Myrmic Common

Wire types shared across the Myrmic WASM host/guest boundary: database, cell
messaging, gateway routing, BLE and web types, serialized with
[`postcard`](https://crates.io/crates/postcard) and
[`serde`](https://crates.io/crates/serde) so the host and a cell agree on how
to (de-)serialize them.

Used by [`myrmic-sdk`](https://crates.io/crates/myrmic-sdk) on the cell side
and by the host-side `sorg` infrastructure.

The `codegen` feature additionally exposes the code generation logic shared
between `myrmic-sdk-macros`' `#[cmd]`/`#[monitor]`/`#[init]` macros and the
host's `myrmic-build` build-time tooling. It is host-only and never enabled
for a wasm cell target.
