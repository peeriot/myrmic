# Myrmic SDK Macros

Proc-macro crate backing [`myrmic-sdk`](https://crates.io/crates/myrmic-sdk):
the `#[cmd]`, `#[monitor]` and `#[init]` attribute macros that export a cell's
functions to the host, the `Message` derive, and the codegen driving the
`myrmic_sdk::import!` macro.

This crate is not meant to be depended on directly - use `myrmic-sdk`, which
re-exports what a cell needs.
