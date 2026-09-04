# ESP WAMR

A firmware ecosystem that uses the WAMR WASM runtime to host functionality for Peeriot's embedded framework

## Supported Chips

* ESP32-C5
* ESP32-C6
* ESP32-C61

## Capability Matrix

The matrix below is the reference for what each supported SoC can do in this firmware ecosystem. This can be used as a
guide for selecting the appropriate ESP SoC for your myrmic application.

| Capability                    | ESP32-C5                                                                             | ESP32-C6                                                                             | ESP32-C61                                                                             |
|-------------------------------|--------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------|
| **ISA / cargo target**        | `riscv32imac`                                                                        | `riscv32imac`                                                                        | `riscv32imac`                                                                         |
| **Internal RAM**              | 384 KB                                                                               | 512 KB                                                                               | 320 KB                                                                                |
| **External RAM (PSRAM)**      | up to 8 MB                                                                           | —                                                                                    | up to 2 MB                                                                            |
| **Flash (max)**               | up to 16 MB                                                                          | up to 8 MB                                                                           | up to 8 MB                                                                            |
| **WiFi**                      | 2.4/5 GHz (11 a/b/g/n/ac/ax)                                                         | 2.4 GHz (11 b/g/n/ax)                                                                | 2.4 GHz (11 b/g/n/ax)                                                                 |
| **Transport — myrmic**        | WiFi                                                                                 | WiFi                                                                                 | WiFi                                                                                  |
| **Transport — user**          | BLE                                                                                  | (BLE) ¹                                                                              | BLE                                                                                   |
| **BLE version**               | 5.3                                                                                  | 5.3                                                                                  | 5.0                                                                                   |
| **BLE host stack**            | NimBLE                                                                               | NimBLE                                                                               | NimBLE                                                                                |
| **BLE Qualification** ²       | Qualifiable                                                                          | Qualifiable ¹                                                                        | Qualifiable                                                                           |
| **Flash AOT default storage** | 2.031 MB (`0x1F0000`)                                                                | 2.031 MB (`0x1F0000`)                                                                | 2.031 MB (`0x1F0000`)                                                                 |
| **Runtime tags (→ myrmic)**   | `esp32c5`, `esp32`, `riscv32imac`, `embedded`, `wifi-myrmic`, `ble`, `gpio`, `psram` | `esp32c6`, `esp32`, `riscv32imac`, `embedded`, `wifi-myrmic`, `gpio`, `i2c`, (`ble`) | `esp32c61`, `esp32`, `riscv32imac`, `embedded`, `wifi-myrmic`, `ble`, `gpio`, `psram` |
| **Implementation details**    | —                                                                                    | Direct I2C bus is exposed to cells.                                                  | —                                                                                     |

¹ The ESP32-C6 can run a BLE transport, but the `esp32c6` firmware feature does **not** enable the `ble` feature by 
default, so BLE is not compiled in and the `ble` runtime tag is not emitted. This is because the BLE requires a 
considerable amount of RAM. Enabling the BLE is possible, but it reduces drastically the complexity of cells that can be
hosted on the SoC. BLE can be enabled it by enabling the `"ble"`  feature in [`modem-esp32/Cargo.toml`](modem-esp32/Cargo.toml).

² The implementation of BLE on the supported chips is based on the combination of the Espressif controller blob and the
NimBLE host stack. This is done so that BLE certification is possible by using qualified or qualifiable components. This
allows the use of BLE in commercial products, **however**, Peeriot does not provide any certification for the BLE 
implementation, and it is the responsibility of the user to ensure compliance with the Bluetooth SIG.

**Runtime tags** are the tags that the device reports to myrmic during registration. Those tags are available to 
select/filter devices for myrmic applications.

## WASM Operation

Currently, to maximize speed and minimize RAM requirements, the ecosystem operates with AOT XIP mode. This allows to 
statically link in the WASM module, storing in FLASH. WAMR then will load the module directly from FLASH, minimizing
trampolines and RAM requirements for executable code while leaving the WASM module binary unmodified.

### Choose a WASM module

The AOT module is not compiled into the firmware — it is stored in a dedicated flash region and loaded at runtime by
`wasm-storage` (either written out-of-band with `espflash write-bin`, or delivered over the air). Use the `aot-compiler`
tool (in `sdk/tools/aot-compiler`) to produce it — it wraps `wamrc` with the correct flags per target and also
generates the `.meta` file required by the firmware loader.

For example, say that we want to load the `blinky` example from the `../tests/fixtures` directory and run it on
the ESP32-C6. One should:

```shell
$ cargo +nightly build --manifest-path ../../Cargo.toml -p blinky --target wasm32-unknown-unknown --release
$ cargo run --manifest-path ../../sdk/tools/aot-compiler/Cargo.toml -- \
    --target esp32c6 \
    --out-dir ../../target/wasm32-unknown-unknown/release/ \
    ../../target/wasm32-unknown-unknown/release/blinky.wasm
$ cargo run --target riscv32imac-unknown-none-elf --features esp32c6 --no-default-features
```

`aot-compiler` requires `wamrc` to be on `$PATH` ([build instructions](https://wamr.gitbook.io/document/wamr-in-practice/tutorial/build-tutorial/build_wamrc))
and to be built from the WAMR version 2.4.4.
It automatically passes `--xip` and the correct `--cpu-features` for the selected target, and writes both
`<name>.aot` and `<name>.meta` to the output directory.

The flags it encodes per target are:

| SoC       | `--cpu-features` | `cargo` target                                             |
|-----------|-----------------|------------------------------------------------------------|
| ESP32-C5  | `+i,+m,+a,+c`   | `--target riscv32imac-unknown-none-elf --features esp32c5` |
| ESP32-C6  | `+i,+m,+a,+c`   | `--target riscv32imac-unknown-none-elf --features esp32c6` |
| ESP32-C61 | `+i,+m,+a,+c`   | `--target riscv32imac-unknown-none-elf --features esp32c61`|

### Tuning the firmware / AOT flash split

The flash is split between the **firmware** (the app) and the **AOT module storage**. By default (no config file), a
4 MB layout is used on every chip. **NOTE** This might actually reduce the capacity that might be available for you on
the SoC. If you SoC has more/less flash than 4 MB, you should create a `partitions.toml`. The default layout is:

| Region                | Offset     | Size                  |
|-----------------------|------------|-----------------------|
| Firmware & Bootloader | `0x000000` | 2.097 MB (`0x200000`) |
| AOT metadata          | `0x200000` | 64 KB (`0x10000`)     |
| AOT XIP module        | `0x210000` | 2.031 MB (`0x1F0000`) |

To rebalance — give the firmware more room, store a larger module, or use the full flash of a larger device — create
[`modem-esp32/partitions.toml`](modem-esp32/partitions.toml) and set the two knobs (`firmware_size`, `aot_size`, plus an
optional `flash_size`). `modem-esp32`'s build script reads it and:

- generates the `PartitionLayout` injected into `wasm-storage` (so the runtime maps the AOT region at the right place),
  and
- generates an **app-only** ESP-IDF partition table (the AOT region is left as an unpartitioned gap, mapped manually by 
  `esp-mmu` and mutated at runtime). Because the firmware ends exactly at the AOT boundary, `espflash` refuses to flash 
  a firmware image that would overflow into the AOT region — enforcing the ceiling with the **stock bootloader**, no custom bootloader required.

An example partition file can be found in `embedded/esp-hal/modem-esp32/partitions.toml.example`

## Signal Layer

The Signal Layer — the native layer of sensor drivers, processing steps, and the tap registry that
feeds the WASM cells — is documented separately. See
[`sdk/signal-layer/README.md`](../../sdk/signal-layer/README.md) for the crate map and
[the Signal Layer handbook chapter](../../doc/chapters/05_guides/11_signal-layer.md) for the
model and its rationale. The
ESP32-specific board manifests, pipelines, and the `esp-codegen` generator live under
[`signal-layer/`](signal-layer/).

## Memory Configuration

The WASM runtime on embedded requires careful tuning across several independent memory regions. When things go wrong they tend to produce confusing errors that are superficially similar, so understanding the layers is essential.

### Memory layers

| Layer | Where configured | Purpose |
|---|---|---|
| **Main heap** | `esp_alloc::heap_allocator!(size: N * 1024)` in `main.rs` | All dynamic allocations: WAMR internals, WASM linear memory, exec envs |
| **RTOS task stack** | `stack_size` in `embassy_executor::task` | Native call stack for the firmware task hosting WAMR |
| **WASM linear memory** | `--initial-memory=N` in `wamrc` / WASM `memory` section | WASM module's address space, allocated from the main heap at instantiation time |
| **WAMR exec env stack** | `stack_size` in `wasm_runtime_create_exec_env()` | Operand stack / overflow sentinel per function call, allocated from the main heap |
| **WASM managed heap** | `init_allocator()` in the WASM module itself | Module-level allocations inside WASM (walloc, etc.) — carved from linear memory, **not** the main heap |

### Diagnosing allocation errors

#### `os_mmap FAILED` during module load — usually harmless
```
ERROR - os_mmap FAILED size=69632
INFO  - Module loaded        ← still succeeds!
```
WAMR tries to copy the AOT code to RAM for relocation. In XIP (execute-in-place) mode the module runs directly from flash-mapped memory, so the failed copy is not used. The module loads successfully. This log line can be ignored.

#### `allocate linear memory failed` during instantiation
```
ERROR - Instantiation failed: AOT module instantiate failed: allocate linear memory failed
```
WAMR could not allocate the WASM module's initial linear memory from the main heap. Causes:
- **Main heap too small** — increase `esp_alloc::heap_allocator!(size: ...)` in `main.rs`.
- **`--initial-memory` too large** — reduce the WASM module's initial memory in `wamrc` or the WASM source.

#### `failed to create exec environment`
```
ERROR - WAMR engine initialization failed: failed to create exec environment
```
`wasm_runtime_create_exec_env()` failed, meaning the main heap is exhausted *after* successful instantiation. The exec env struct + its stack is allocated from the same heap. Increase the main heap.

### Typical values (ESP32-C6, WiFi enabled)

```rust
// main.rs — this needs to be large enough for:
//   WiFi driver buffers     ~50 KB
//   WAMR internals          ~10 KB
//   WASM linear memory      ~64 KB  (depends on --initial-memory)
//   WAMR exec env stacks    ~16 KB  (2 × 8 KB for init_heap + run_task)
//   Misc allocations        ~20 KB
esp_alloc::heap_allocator!(size: 280 * 1024);
```

If you reduce the WASM module's `--initial-memory` (e.g. to 64 KB instead of 128 KB), you can lower this accordingly.

## Debugging with serial printouts

One can simply make use of `espflash` (installable via `cargo install espflash --locked`), by running one of the aliased
commands in the `embedded/esp-hal` folder.

* `cargo run-c5`: Uses `espflash --monitor` to debug the ESP32-C5 
* `cargo run-c6`: Uses `espflash --monitor` to debug the ESP32-C6
* `cargo run-c61`: Uses `espflash --monitor` to debug the ESP32-C61

## Debugging with probe-rs

Debugging and flashing with probe-rs is pretty straight forward, there is just one important flag, that has to be disabled: `haltAfterReset`
With the flag active we end up in the bootloader and after some stepping or running the debugger get's out of sync.
So the recommended way is to add a breakpoint manually at the entry point.

It's also recommended to disable LTO and optimizations and use the debug build:
```toml
[profile.dev]
debug         = true
lto           = false
opt-level     = 0
codegen-units = 1
```

### Via VSCode
```json
{
    "version": "0.2.0",
    "configurations": [
        {
            "type": "probe-rs-debug",
            "request": "launch",
            "name": "esp-wamr debug (esp32c6)",
            "cwd": "${workspaceFolder}",
            "connectUnderReset": true,
            "chip": "esp32c6",
            "flashingConfig": {
                "flashingEnabled": true,
                "haltAfterReset": false
            },
            "coreConfigs": [
                {
                    "coreIndex": 0,
                    "programBinary": "./target/riscv32imac-unknown-none-elf/debug/esp-wamr"
                }
            ]
        }
    ]
}
```

### Via `probe-rs qemu`
```sh
probe-rs gdb --chip esp32c6 --connect-under-reset target/riscv32imac-unknown-none-elf/debug/modem-esp32
```
