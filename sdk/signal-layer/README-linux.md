# sdk/signal-layer

Linux-side crates for FEAT-2026-SIG-002 (Signal Layer on Linux).

| Crate | Role |
|---|---|
| `signal-layer-ipc` | IPC protocol types, framing, versioned tap server and client |
| `signal-layer-linux-rt` | Fenced time seam (`time::now_millis`) and IPC-server bootstrap for generated pipelines |
| `linux-i2c-shim` | `embedded_hal_async::i2c::I2c` over blocking `i2cdev` via `spawn_blocking` |
| `linux-codegen` | `LinuxChipBackend` codegen CLI; generates a tokio pipeline crate from a pipeline YAML + Linux manifest |

## D7 probe result (collision confirmed; test-filter remediation applied)

`cargo test --workspace --no-run` produces a compile error even before Task 1 changes:

```
error: You must set at most one of these Cargo features:
  restore-state-none, restore-state-bool, restore-state-u8, restore-state-u16,
  restore-state-u32, restore-state-u64, restore-state-usize
```

This is a **pre-existing** workspace-level `critical-section` feature conflict:
`esp-hal` forces `restore-state-u32`; `embassy-sync`, `once_cell`, and several
other host-target crates force `restore-state-bool` (via `critical-section/std`).
Adding `critical-section/std` to `signal-layer-linux-rt` would make no difference
to this pre-existing conflict.

**Remediation applied (test-filter path):** `critical-section/std` is NOT declared in
`signal-layer-linux-rt`'s `Cargo.toml`.  It belongs only in the **generated** pipeline
binary (Task 10), which is a standalone Cargo project outside this workspace and will
never see `esp-hal` in its dependency tree, so no conflict arises there.

Signal-layer Linux crate tests must be invoked per-package rather than `--workspace`:

```sh
cargo test -p signal-layer-ipc -p signal-layer-linux-rt -p linux-i2c-shim
```
