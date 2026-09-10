# firmware-examples

Worked examples of building a myrmic firmware on
[`esp-firmware`](../crates/esp-firmware/). Also the template: copy this
directory to start a new firmware crate.

| Example | Shows |
| ------- | ----- |
| [`default.rs`](src/bin/default.rs) | The whole firmware, unmodified — what `modem-esp32` is. |
| [`own-ble`](src/bin/own-ble.rs) | Replacing the shipped BLE stack, and keeping a GPIO back from the cell. |
| [`native-cell`](src/bin/native-cell.rs) | Being the cell natively, with no WASM runtime at all. |
| [`taps.rs`](src/bin/taps.rs) | Publishing a tap and acting on an outlet, declared by the firmware rather than a pipeline. |

```sh
cd embedded/esp-hal
cargo build -p firmware-examples --bin native-cell \
  --release --target riscv32imac-unknown-none-elf \
  --no-default-features --features esp32c6 -Zbuild-std=core,alloc
```

## What makes this a firmware crate

Four things, all visible here:

1. **`Cargo.toml`** declares chip features that forward to `esp-firmware`, and
   depends on `esp-firmware` — nothing else is needed for `default.rs`. The
   other two examples add `embassy-executor` for `#[embassy_executor::task]`.
2. **`build.rs`** is one call to `esp_firmware_build::configure()`.
3. Each binary is `#![no_std]` + `#![no_main]`, which an attribute macro cannot
   add for you.
4. The workspace `.cargo/config.toml` supplies the riscv target rustflags. A
   standalone project needs its own, plus a `nimble-config.toml` at the
   workspace root if it uses BLE.

A firmware that registers a native cell never loads a module, so the AOT region
in its `partitions.toml` can shrink and the firmware partition can grow to take
the space. The build script warns below 200 KB of AOT — expected here, since
nothing will be stored in it.
