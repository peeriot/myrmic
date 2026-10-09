# myrmic cells classes add

## Name
`myrmic cells classes add` - Register a cell class manually

## Synopsis
```
myrmic cells classes add [OPTIONS] [NAME]
```

## Description
Registers a cell class from a binary artifact and names it `NAME`, or adds an AOT binary under `NAME`. Classes are normally created automatically by [`myrmic deploy`](../../05_deploy.md) - use this command to register pre-built artifacts directly.

A class is identified by the hash of its binary; the name is a label for it. Several names can point to the same class, and binding a name that already points to another class moves the name - the other class stays registered.

## Options
`--wasm PATH`

Path to a `.wasm` binary to register as a class. Mutually exclusive with `--aot`.

`--aot PATH`

Path to an AOT-compiled binary. Requires `--platform`. If `NAME` points to a wasm class, the binary is added to that class as its compilation for the platform. Otherwise it is registered as a class of its own, and `NAME` is bound to it.

`--platform PLATFORM`

Target platform for the AOT binary. Accepted values:
- `riscv32imac` - Espressif RISC-V chips (ESP32-C5, ESP32-C6, ESP32-C61). Also accepted: `esp32c5`, `esp32_c5`, `esp32c6`, `esp32_c6`, `esp32c61`, `esp32_c61`.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Register a class from a wasm binary:

```bash
myrmic cells classes add my-cell --wasm ./my-cell.wasm
```

2. Add its AOT compilation for ESP32-C6 boards:

```bash
myrmic cells classes add my-cell --aot ./my-cell.aot --platform esp32c6
```
