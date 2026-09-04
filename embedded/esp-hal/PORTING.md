# Porting a new ESP chip

This document describes the rough steps required to bring up a new Espressif SoC (an ESP32 RISC-V variant) in this
firmware ecosystem.

The work splits into two halves:

1. **Embedded firmware** (`embedded/`) — the code that actually runs on the chip: cargo aliases, the MMU driver, GPIO
   mappings, storage/partition layout, and heap/PSRAM setup.
2. **OS swarm layer** (`swarm/`, `sdk/`) — the host-side orchestration and tooling that must learn the new target so
   cells can be built, tagged, placed, and deployed to it.

A new target does not deploy end-to-end until *both* halves know about it. It's easiest to land the firmware first (so
you can build and flash) and the swarm/tooling changes second (so the deploy path lights up), but they are largely
independent.

Throughout, the codebase gates chip-specific code with cargo features named after the SoC (`esp32c5`, `esp32c6`, 
`esp32c61`, …) and selects between them with `cfg_match!` /
`#[cfg(feature = "…")]`. Porting is largely a matter of finding every such `cfg_match!` /
`compile_error!("only … supported")` site and adding an arm for your chip. `grep`-ing for an existing target name
(`esp32c6`) across the repo is the fastest way to enumerate the sites that need touching.

---

## Prerequisites: know your chip

Before writing code, gather the hardware facts the port depends on. For the C61 these were:

- **ISA / cargo target triple.** RISC-V ISA extensions determine the target triple. C3 is `riscv32imc`
  (no atomics → `riscv32imc-unknown-none-elf`); C6 and C61 are `riscv32imac` (with atomics →
  `riscv32imac-unknown-none-elf`). This also decides the AOT compiler `--cpu-features` string (see
  [Build & AOT tooling](#10-build--aot-tooling)) and the `ArtifactTarget` your cells are keyed under — AOT artifacts
  are shared across every chip of the same ISA, so a chip whose ISA is already supported ships **no new artifact
  variant** (see [Target / artifact enums](#8-target--artifact-enums)).
- **MMU geometry.** Max page number, page size register layout, valid-bit position, and the IBUS/DBUS base addresses.
  These differ per chip and are the crux of the MMU driver.
- **PAC crate. (maybe required for some chips)** Sometimes, the peripherals that are needed to operate on the MMU are
  not directly available through `esp-hal`. As an alternative (e.g. like the CLIC for the "esp32c61" port) there might
  be the need to use directly a peripheral-access crate for the chip.
- **Cache ROM functions.** The names of the ROM cache-freeze functions (`Cache_Freeze_ICache_Enable` vs
  `Cache_Freeze_Enable`, …). This information is available by scanning the `esp-idf` toolchain to check what the
  existing C drivers are disabling/enabling the cache with.
- **Interrupt controller.** PLIC vs CLIC vs the C3's `INTERRUPT_CORE0` — this changes how interrupts are saved/restored
  around uncached MMU sections. Each chip might have a different way to operate on the interrupt controller.
- **Memory map / PSRAM.** Usable DRAM size (drives the heap budget), whether the chip has PSRAM, and whether PSRAM
  shares the MMU page space with XIP (the C61 does — see
  [PSRAM & heap](#4-heap--psram-setup)).
- **BLE support.** Whether the target has a radio and whether `esp-nimble-host` supports it.

Much of this comes from Espressif's technical reference manual, the `esp-idf` C toolchain and the `esp-hal` /
`esp-metadata` crates.

---

## Firmware (`embedded/`)

### 1. Cargo command aliases

Add build/run/clippy/etc. aliases for the new chip so day-to-day commands are short.

**File:** [`.cargo/config.toml`](../../.cargo/config.toml)

Copy the block of `*-c6` aliases and rename to `*-*`, adjusting the `--target` triple and the `--features` flag:

e.g. for the C61:

```toml
check-c61 = "check  -p modem-esp32 --release --target riscv32imac-unknown-none-elf --no-default-features --features esp32c61 -Zbuild-std=core,alloc"
build-c61 = "build  -p modem-esp32 --release --target riscv32imac-unknown-none-elf --no-default-features --features esp32c61 -Zbuild-std=core,alloc"
# … run-c61, clippy-c61, doc-c61, citest-c61
```

After this, `cargo +nightly build-*` should at least start compiling (and fail on the missing feature arms you're about
to add).

### 2. Feature plumbing

Each embedded crate exposes a per-chip cargo feature that forwards to the same feature on its dependencies. Add an
`esp32c61` feature to every crate in the dependency chain:

- [`modem-esp32/Cargo.toml`](modem-esp32/Cargo.toml) — the firmware binary. Add an `esp32*`
  feature forwarding to all the dependencies that might need that information (e.g. `esp-backtrace/esp32*`,
  `wasm-storage/esp32*`, `wasm-runtime/esp32*`, etc...).
- [`crates/wasm-runtime/Cargo.toml`](crates/wasm-runtime/Cargo.toml) — forwards to `wasm-storage/esp32*`.
- [`crates/wasm-storage/Cargo.toml`](crates/wasm-storage/Cargo.toml) — forwards to `esp-mmu/esp32*`.
- [`crates/esp-mmu/Cargo.toml`](crates/esp-mmu/Cargo.toml) — `esp-hal/esp32*`, plus the PAC crate if needed.
- [`crates/esp-watchdog/Cargo.toml`](crates/esp-watchdog/Cargo.toml) — forwards to `esp-hal/esp32*`.
- [`crates/cell-db-service/Cargo.toml`](crates/cell-db-service/Cargo.toml) — forwards to `esp-hal`, `esp-watchdog` and `wasm-runtime`.

If you pulled in a new PAC crate or bumped a fork branch, also register it in the workspace root
[`Cargo.toml`](../../Cargo.toml) `[workspace.dependencies]` (e.g. `esp32c61 = "0.3.2"`), and update any git-fork branch
pins (the C61 port required an `esp-nimble-host` patch to use BLE). This regenerates `Cargo.lock`.

### 3. MMU driver

**File:** [`crates/esp-mmu/src/lib.rs`](crates/esp-mmu/src/lib.rs)

This is the hardest part of the port — it maps the AOT WASM module out of flash for execute-in-place (XIP). The driver
is a collection of `cfg_match!` blocks keyed on the chip feature. Add a `feature = "esp32c61"` arm to each. As an
example, for the C61 this meant:

- **Constructor.** Add a `mmu_from_peripherals!` arm and a matching `Mmu::new(...)` constructor taking the peripherals
  the chip's MMU uses (`SPI0` on C6/C61; `INTERRUPT_CORE0` on C3; the C61 does *not* take a `PLIC_MX` because it uses a
  CLIC).
- **`MAX_PAGE_NUMBER`** — C3: 128, C6: 256, C61: 512.
- **Page size** — the C61 reads the same `mmu_power_ctrl` register block as the C6, but the register field is named
  `mmu_page_size` rather than the C6's `spi_mmu_page_size`.
- **`VALID_BIT`** — C3: `1`, C6: `1 << 9`, C61: `1 << 10`.
- **`DBUS_BASE`** — the C61 shares the C6's `IBUS_BASE` for data.
- **Interrupt save/restore around uncached sections.** This is chip-specific. The C6 uses a PLIC with a single enable
  register; the C61 uses a **CLIC** with no single enable register, so each external interrupt line's `int_ie` bit must
  be saved and disabled independently (external lines start at CLIC index 16). This is where the `esp32c61` PAC crate is
  used (`CLIC::steal()`).
- **Cache freeze ROM functions.** The C61's ROM exposes `Cache_Freeze_Enable` /
  `Cache_Freeze_Disable` rather than the C3/C6's `Cache_Freeze_ICache_Enable` /
  `Cache_Freeze_ICache_Disable`. Add arms in `cache_stop` / `cache_start`. `cache_invalidate_addr`
  uses the common `Cache_Invalidate_Addr` symbol — just add the chip to the `cfg!` guard.
- Update the `compile_error!` / `unimplemented!("Only … supported")` messages to mention the new chip.

> **`unsafe` note.** New `unsafe` blocks here (PAC `steal()`, MMIO, ROM FFI) each need a `// SAFETY:`
> comment — the workspace denies `undocumented_unsafe_blocks`. Run the `unsafe-auditor` agent over
> the driver, and the `concurrency-auditor` over the interrupt save/restore path.

### 4. GPIO mappings

**File:** [`crates/wasm-runtime/src/imports/gpio.rs`](crates/wasm-runtime/src/imports/gpio.rs)

The runtime exposes a fixed-size array of GPIO pins to WASM cells. Add:

- The `InnerPins` array length for the chip (C3: 11, C6: 28, C61: 30) under `#[cfg(feature = "esp32*")]`.
- A `#[cfg(feature = "esp32*")]` arm in the `pins_from_peripherals!` macro that builds the `Pins`
  array. Use `Some(Flex::new($periph.GPIOn))` for pins safe to expose and `None` for pins that are reserved (e.g.
  strapping pins, flash/PSRAM pins).
- Extend the fallback `compile_error!("Only … supported")` arm's `cfg(not(any(…)))` guard and message to include the new
  chip.

### 5. Storage & partition layout

**Files:** [`crates/wasm-storage/src/partitions.rs`](crates/wasm-storage/src/partitions.rs),
[`modem-esp32/build.rs`](modem-esp32/build.rs)

**PSRAM caveat (C61-specific but reusable for other PSRAM chips).** If the chip has PSRAM, PSRAM and XIP share the same
MMU page space, and PSRAM occupies the lowest free pages after boot. Using the physical address directly as the XIP
virtual-address offset would collide with the PSRAM window. Instead, map XIP into the **highest** MMU pages (leaving the
last page reserved for the bootloader). `PartitionLayout` in `partitions.rs` computes `meta_vaddr_offset()` /
`xip_vaddr_offset()` differently for PSRAM vs non-PSRAM SoCs, and
[`storage.rs`](crates/wasm-storage/src/storage.rs) uses those offsets when building the `Region`s instead of the raw
`paddr`. Non-PSRAM chips (`esp32c6`) set the offset to `paddr` directly. Add a `feature = "esp32*"` arm to
the appropriate (PSRAM / non-PSRAM) `impl PartitionLayout` block. (`Mmu::MAX_PAGE_NUMBER` is `pub` so the offset math
can reach it.)

### 6. Heap & PSRAM setup

**File:** [`modem-esp32/src/main.rs`](modem-esp32/src/main.rs)

Chip DRAM sizes differ, so the heap allocator size is per-chip. This lives in the `setup_heap!`
macro:

- Add a `#[cfg(feature = "esp32*")] esp_alloc::heap_allocator!(size: …)` arm. **Size it against the stack.** The heap
  lives in `.bss` and shares the RAM region with the main executor stack, so every KB of heap is a KB off the stack. The
  C61 budget was 96 KB, leaving ~16 KB of stack (~1.6× the ~10 KB peak). Re-check the high-water mark (the `stack-hwm`
  feature) if you change it.
- **Reclaim `dram2_seg`** (the 64 KB second region) if the chip has it — reuse the C6 arm.
- **PSRAM.** If the chip has PSRAM, initialize it (`Psram::new(periphs.PSRAM, PsramConfig::default())`), register the
  mapped window as an `External`-capability heap region *after* the internal heap (so internal allocations are preferred
  and DMA-only allocations stay out of PSRAM), and
  `core::mem::forget` the handle to keep the mapping alive.

> `setup_heap!` takes the `peripherals` handle and must run *after* `esp_hal::init(...)` (PSRAM needs
> the `PSRAM` peripheral). Note the ordering in `main`: `esp_hal::init` → `setup_heap!` → logger.

### 7. CI

**File:** [`.github/workflows/push-validation.yml`](../../.github/workflows/push-validation.yml)

Add a `check_*` job (copy `check_c6`) that runs the `.ci/check/check` script with
`COMMAND_SUFFIX: -*` (this selects the `*-*` cargo aliases), `CARGO_CHANNEL: nightly`,
`SKIP_TESTS: true`. Add a matching `check_*` `workflow_dispatch` input so the job can be triggered manually.

---

## OS swarm layer & tooling (`swarm/`, `sdk/`)

The host side must learn the target so cells can be built, tagged, placed, and deployed. Most of these are plain enum
additions — the compiler's exhaustiveness checking points you at every match arm that needs a new branch. Add the
variant, then follow the errors.

> **Exception: `ArtifactTarget` is keyed by ISA class, not by chip.** The AOT bytes depend only on the
> `--cpu-features`, so all `riscv32imac` chips (C6, C61, …) share one artifact. A chip whose ISA already has a variant
> (`Riscv32imc` / `Riscv32imac`) adds **no** `ArtifactTarget` variant, so exhaustiveness will *not* flag the sites that
> consume it — you route the new chip into the existing ISA variant by hand. `grep` for the ISA name
> (`Riscv32imac`) to find them.

### 8. Target / artifact enums

- [`swarm/myrmic-tags/src/lib.rs`](../../swarm/myrmic-tags/src/lib.rs) — add `Target::Esp32*`, its tag list, and the
  `TryFrom<&str>` arm. The tag list carries **both** the chip name and the ISA class
  (`["esp32*", "esp32", "riscv32im*", "embedded"]`) — the orchestrator resolves the device's `ArtifactTarget` from the
  ISA tag, so it must be present.
- [`swarm/cell-protocol/src/lib.rs`](../../swarm/cell-protocol/src/lib.rs) — `ArtifactTarget` is keyed by **ISA class**,
  not by chip (all `riscv32imac` chips share one artifact). If your chip's ISA already has a variant (`Riscv32imc` for
  C3; `Riscv32imac` for C6/C61) you add **no new variant** — just extend the `FromStr` arm so the chip's names
  (`"esp32*"` and `"esp32_*"`) resolve to the existing ISA variant. Only a chip that introduces a *new* ISA needs a new
  `Riscv32*` variant, with its `as_str()` returning the ISA string (e.g. `"riscv32imac"`).
- [`swarm/cell-protocol/src/exec_runtime.rs`](../../swarm/cell-protocol/src/exec_runtime.rs) — add a
  `RuntimeKind::Esp32*` variant and map `Target::Esp32* → RuntimeKind::Esp32*` (a target known to myrmic but without a
  deploy path maps to `RuntimeKind::Unknown` instead — that's the fallback for chips the orchestrator can't yet deploy
  to).

### 9. Orchestration deploy path

In [`swarm/sorg-orchestration/`](../../swarm/sorg-orchestration/), route the new `RuntimeKind` down the embedded (not
Linux) path:

- `event_loop/cells/deploy/deploy_cell/mod.rs` — add `RuntimeKind::Esp32*` to the
  `=> deploy_wasm_cell_embedded(...)` arm.
- `event_loop/cells/undeploy/mod.rs` — same for `undeploy_wasm_cell_embedded`.
- `event_loop/cells/deploy/placement/preprocessing/mod.rs` — the `artifact_rejection` arm rejects placement unless the
  cell ships an AOT artifact for the runtime's **ISA class** (`ArtifactTarget::Riscv32im*`). Chips of the same ISA share
  one arm — e.g. `RuntimeKind::Esp32c6 | RuntimeKind::Esp32c61 => … ArtifactTarget::Riscv32imac`. If your chip's ISA is
  already handled, add its `RuntimeKind` to the existing arm's pattern rather than writing a new arm.

### 10. Build & AOT tooling

- [`swarm/myrmic-build/src/build.rs`](../../swarm/myrmic-build/src/build.rs) — map
  `Target::Esp32* → aot_compiler::Target::ESP32*`.
- [`sdk/tools/aot-compiler/src/lib.rs`](../../sdk/tools/aot-compiler/src/lib.rs) — add
  `Target::ESP32*` and put it in the correct `--cpu-features` group. The C61 groups with C5/C6 (`riscv32`, `ilp32`,
  `+i,+m,+a,+c`); the C3 is the `imc` group without atomics. Getting the CPU features wrong produces an AOT that links
  but faults at runtime.

### 11. Firmware runtime identity

**File:** [`crates/cell-db-service/src/myrmic.rs`](crates/cell-db-service/src/myrmic.rs)

`create_runtime_info` reports what the device *is* to the swarm. Add `esp32*` arms setting the human name (`"ESP32-*"`)
and `Target::Esp32*`. Also add the target arms in
[`crates/cell-db-service/src/deploy.rs`](crates/cell-db-service/src/deploy.rs)
so the device fetches the AOT + `.meta` artifacts scoped to its own ISA class (`ArtifactTarget::Riscv32im*`).

### 12. HIL tests

**File:** [`embedded/hil-tests/tests/integration/mod.rs`](hil-tests/tests/integration/mod.rs)

Add the new target to the three helpers that switch on the `EMBEDDED_TARGET` env var (`aot_target`, `build_platform`,
`artifact_platform`) so the hardware-in-the-loop suite can build and flash cells for the chip.

---

## Checklist

Firmware:

- [ ] `.cargo/config.toml` — `*-*` aliases
- [ ] Per-crate `esp32*` cargo features (`modem-esp32`, `esp-common`, `wasm-runtime`, `wasm-storage`, `esp-mmu`, `esp-watchdog`, `cell-db-service`, `esp-network`)
    + workspace deps / PAC crate / fork branch pins
- [ ] `esp-mmu` — every `cfg_match!` arm (constructor, page number/size, valid bit, bus base, interrupt save/restore,
  cache ROM fns, error messages)
- [ ] `wasm-runtime/imports/gpio.rs` — pin array length + `pins_from_peripherals!` arm
- [ ] `wasm-storage/partitions.rs` — partition constants (+ PSRAM vaddr-offset math if applicable)
- [ ] `esp-watchdog/src/watchdog.rs` — `record_storage` backend for the hang record (`rtc_fast` persistent RAM, or
  LP_AON scratch registers where the chip has none)
- [ ] `main.rs` `setup_heap!` — heap size, dram2 reclaim, PSRAM init
- [ ] CI `check_*` job + dispatch input

Swarm / tooling:

- [ ] `myrmic-tags` (chip tag **+ ISA tag**), `cell-protocol` `RuntimeKind` enum (+ `ArtifactTarget` only if the chip
  introduces a new ISA)
- [ ] `sorg-orchestration` deploy / undeploy / placement arms
- [ ] `myrmic-build` + `aot-compiler` target mapping & CPU features
- [ ] `cell-db-service` runtime identity (`myrmic.rs`, `deploy.rs`)
- [ ] `hil-tests` target helpers

Finally, run the entire set of HIL tests for the new hardware target. If they pass, the port is complete:

```
$ EMBEDDED_TARGET=ESP32C61 cargo nextest run -p hil-tests --no-fail-fast --no-capture
```
