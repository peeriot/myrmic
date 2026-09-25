---
sidebar_label: Installation (Embedded)
---

# Installation (Embedded)

This page adds what you need to work with ESP32 boards: building and flashing Myrmic **firmware**, and compiling your own **cells** for the board. Firmware is a native RISC-V binary - a Node that runs on the bare-metal chip and joins the swarm - so unlike a cell it is not compiled to WebAssembly. You build it with `myrmic build` and write it to a board with `myrmic flash`. Cells stay WebAssembly, but to run on the device they are ahead-of-time compiled to native RISC-V with `wamrc`; that tool is the one piece you build by hand, covered under [wamrc](#wamrc-the-cell-aot-compiler) below.

Everything on the [Installation](./01_installation.md) page comes first: the **Myrmic CLI**, Rust via rustup, and the C toolchain. This page assumes the CLI is already on your `PATH` (`myrmic --version` works) and only covers the extra pieces the embedded target needs.

## Supported chips

| Chip | Architecture | Wasm engine | Connects over |
|---|---|---|---|
| ESP32-C5 | RISC-V (rv32imac) | WAMR (AOT) | native USB-Serial-JTAG |
| ESP32-C6 | RISC-V (rv32imac) | WAMR (AOT) | native USB-Serial-JTAG |
| ESP32-C61 | RISC-V (rv32imac) | WAMR (AOT) | native USB-Serial-JTAG |

All three connect over their native USB-Serial-JTAG: plug the board's **USB** port into your machine and it enumerates as a USB CDC-ACM device, `/dev/ttyACM*` on Linux. Many devkits also carry a second, **UART** port through an onboard USB-to-UART bridge, which comes up as `/dev/ttyUSB*`. Either port can flash the board; the examples here use the USB port (`/dev/ttyACM*`).

## Prerequisites

On a fresh Debian or Ubuntu image, run `sudo apt update` once before the first `sudo apt install` on this page.

### Clang, for the embedded Wasm runtime

The firmware embeds the WAMR WebAssembly runtime, whose C sources are cross-compiled to the chip's RISC-V target as part of the build. That step uses **clang** - the C compiler the build invokes for the bare-metal target, since a host `gcc` cannot cross-compile to `riscv32imac` - and **libclang**, which the binding generator loads at build time. Without them the first `myrmic build` stops with `failed to find tool "clang"`.

| Distribution family | Install |
|---|---|
| Debian, Ubuntu | `sudo apt install clang libclang-dev` |
| RHEL, AlmaLinux, Fedora | `sudo dnf install clang clang-devel` |

`clang` and `libclang-dev` are LLVM components, and `libclang-dev` pulls in the LLVM runtime it needs; the heavier `llvm-dev` package (with `llvm-config` and the LLVM static libraries) is **not** required for the firmware - it is only needed to build [wamrc](#wamrc-the-cell-aot-compiler) later.

The host C toolchain from the [Installation](./01_installation.md) page (`build-essential`) is still required as well; it compiles the build scripts that run on your machine. `cmake`, `pkg-config`, `libudev-dev` and a separate RISC-V GCC are **not** needed - the firmware links with the `rust-lld` linker that ships inside the Rust toolchain, and `myrmic flash` has its flasher built in (see below).

### The embedded Rust toolchain

Firmware compiles for `riscv32imac-unknown-none-elf` with a pinned nightly toolchain, the `rust-src` component, and Rust's `-Z build-std`. You do not install these by hand: a firmware crate scaffolded by `myrmic new --firmware`, on its first `myrmic build`, ships a `rust-toolchain.toml` that prompts `rustup` to auto-install the toolchain, its `rust-src` component and the target. You will see a line like `the missing active toolchain nightly-2026-08-07 has been auto-installed` the first time.

As with cells, the pinned nightly date is the latest tested for the current Myrmic release and moves forward with each release. To install it ahead of that first build - on an offline machine, or just to keep the build output quiet - the steps are optional:

```sh
rustup toolchain install nightly-2026-08-07
rustup component add rust-src --toolchain nightly-2026-08-07
rustup target add riscv32imac-unknown-none-elf --toolchain nightly-2026-08-07
```

### wamrc, the cell AOT compiler

Firmware is only half of embedded. To run one of your own cells on the board, Myrmic ahead-of-time compiles the cell's WebAssembly to native RISC-V with **wamrc**, the WAMR AOT compiler - both `myrmic build --platform riscv32imac` and `myrmic deploy --platform riscv32imac` call it. Building and flashing firmware does not, so if that is all you do you can skip this section.

wamrc is not packaged anywhere; you build it once from the WAMR sources, against the LLVM your distribution already ships. Only wamrc is compiled - not LLVM - so this is a short build, but the LLVM packages it links against are a few hundred megabytes to download. Myrmic requires **wamrc 2.4.4** exactly and rejects any other version. WAMR 2.4.4 is built against **LLVM 18**, which is what Ubuntu 24.04 installs as its default `llvm-dev` - so on 24.04 there is no version to choose.

Install the build tools:

| Distribution family | Install |
|---|---|
| Debian, Ubuntu | `sudo apt install build-essential cmake ninja-build llvm-dev` |
| RHEL, AlmaLinux, Fedora | `sudo dnf install gcc-c++ cmake ninja-build llvm-devel` |

The one value to get right is the LLVM directory the build links against. It must be the `llvm-dev` you just installed, not some other `llvm-*` directory that happens to exist - a leftover runtime-only directory (for example one holding just `libc++`) has no CMake config and fails late and cryptically. List what you have with `ls -d /usr/lib/llvm-*`; on Ubuntu 24.04 it is `/usr/lib/llvm-18`. Then build wamrc and put it on your `PATH`:

```bash
curl -fsSL https://github.com/bytecodealliance/wasm-micro-runtime/archive/refs/tags/WAMR-2.4.4.tar.gz | tar -xz
src="$PWD/wasm-micro-runtime-WAMR-2.4.4"
mkdir -p "$src/core/deps/llvm"
ln -sfn /usr/lib/llvm-18 "$src/core/deps/llvm/build"   # the llvm-dev you installed
cmake -S "$src/wamr-compiler" -B wamrc-build -G Ninja -DCMAKE_BUILD_TYPE=Release
cmake --build wamrc-build
sudo install -m 755 wamrc-build/wamrc /usr/local/bin/wamrc
```

Verify the version - it must read exactly `2.4.4`, or `myrmic build` refuses it:

```bash
wamrc --version
```

```text
wamrc 2.4.4
```

Two things about the result. wamrc links LLVM dynamically, so it stays on the machine that built it - it is not a binary you can copy to another node, and removing the LLVM runtime it links (`libllvm18`) breaks it. And Myrmic only checks that `wamrc` is on your `PATH`: `/usr/local/bin` above is on `PATH` by default, so if you install it elsewhere (for example `~/.cargo/bin`) make sure that directory is on `PATH`.

`llvm-dev`, `cmake` and `ninja-build` are only needed to *build* wamrc, not to run it, so once it is on your `PATH` you can remove them to reclaim the space (`llvm-dev` and its LLVM development files are the bulk of it):

```bash
sudo apt remove llvm-dev cmake ninja-build && sudo apt autoremove
```

wamrc keeps working afterwards because the LLVM runtime it links, `libllvm18`, stays behind: `libclang-dev` from the Clang step above depends on it, so `autoremove` leaves it in place. Two caveats. If you built wamrc on a machine that does *not* have the firmware prerequisites, nothing else holds `libllvm18` and `autoremove` would take it, breaking wamrc - keep `libllvm18` (or just keep `llvm-dev`) there. And leave `cmake` and `ninja-build` if other tooling on the machine uses them; many Rust crates build C dependencies with CMake.

### git

The scaffolded firmware pins its ESP SDK crates to git revisions, so the first build fetches them from GitHub (the `esp-hal`, `wamr-rust-sdk` and related repositories). Install `git` if it is not already present - it is the same package the [Installation](./01_installation.md) page lists for building the CLI from source.

| Distribution family | Install |
|---|---|
| Debian, Ubuntu | `sudo apt install git` |
| RHEL, AlmaLinux, Fedora | `sudo dnf install git` |

### Serial access, for flashing

`myrmic flash` talks to the board over its serial port; the flasher is compiled directly into the CLI, requiring no extra packages to be installed. What it needs is permission to open the port. On Linux the serial devices belong to the `dialout` group, so add yourself to it once:

```bash
sudo usermod -aG dialout "$USER"
```

Log out and back in for the new group to take effect. Until then, and on systems without that group, `myrmic flash` can only reach the board when run with elevated privileges.

## Build a firmware

With the prerequisites in place, scaffold a firmware crate, build it, and flash it. `--firmware` selects the chip; a bare `--firmware` defaults to `esp32c6`, and a specific chip takes the `=` form:

```bash
myrmic new --firmware=esp32c6 my-node
```

Expected output:

```text
INFO  Creating firmware 'my-node' for esp32c6
```

Build it. The first build is the slow one: it auto-installs the toolchain (above), fetches the ESP SDK crates, and compiles `core`, `alloc` and the WAMR runtime from source. On a small cloud instance that first build took around one and a half to two minutes once the toolchain was downloaded; later builds, with everything cached, are seconds.

```bash
cd my-node
myrmic build
```

Expected output (trimmed):

```text
INFO  Building esp32c6 firmware: .../my-node/Cargo.toml
    Updating git repository `https://github.com/peeriot/esp-hal.git`
   Compiling my-node v0.1.0 (.../my-node)
    Finished `release` profile [optimized] target(s) in Xs
INFO  Firmware: .../my-node/target/riscv32imac-unknown-none-elf/release/my-node
INFO  Partition table: .../partitions.generated.csv
INFO  Flash with: myrmic flash .../my-node
```

A successful `myrmic build` is also how you confirm the toolchain is complete: it needs no board attached, so you can verify the whole embedded setup before any hardware arrives.

Then plug in the board and flash it:

```bash
myrmic flash
```

`myrmic flash` connects to the board first (to read its flash size), then builds and writes the image, and the board resets into it. Add `--monitor` to stream the board's serial output afterwards, and `--port /dev/ttyACM0` to name the port when more than one board is attached.

With no board connected the command stops at the connect step - which is the expected result when you are only checking the tooling:

```text
ERROR No serial ports could be detected (Make sure you have connected a device to the host system. If the device is connected but not listed, try using the `--list-all-ports` flag.)
```

### With BLE support

To run cells that talk to Bluetooth Low Energy peripherals, the firmware has to be built with its `ble` feature, which enables the NimBLE host stack:

- It is enabled automatically for the `esp32c5` and `esp32c61` chips - no extra flag is needed, so `myrmic build` already includes BLE.
- It is off by default for `esp32c6`, so a C6 firmware silently skips the entire BLE path unless the feature is added explicitly.

Add the feature when building a C6 firmware:

```bash
myrmic new --firmware=esp32c6 my-node
cd my-node
myrmic build --features ble
myrmic flash --features ble --monitor
```

Without the feature, BLE support is compiled out of the firmware, so it does not advertise `ble` and cannot serve BLE cells. See [Work with BLE peripherals](../05_guides/12_ble.md) for how a cell uses BLE.

## Next

- [`myrmic new`](../10_reference/02_myrmic-cli/01_new.md), [`myrmic build`](../10_reference/02_myrmic-cli/03_build.md) - the full command reference, including `--firmware --pipeline` to scaffold a firmware with a Signal Layer pipeline.
- [First Steps on an ESP32](../04_tutorials/03_first-steps-embedded.md) - flash this firmware to a board and deploy your first cell onto it.
- [First Steps with the Signal Layer](../04_tutorials/04_first-steps-signal-layer.md) - scaffold a pipeline, a cell, and a firmware, and run them together.
