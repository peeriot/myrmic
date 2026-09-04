# myrmic cells classes add

## Name
`myrmic cells classes add` - Register a cell class manually

## Synopsis
```
myrmic cells classes add [OPTIONS] [NAME]
```

## Description
Creates a cell class from a binary artifact, or adds an artifact to an existing class. Classes are normally created automatically by [`myrmic deploy`](../../05_deploy.md) - use this command to register pre-built artifacts directly.

## Options
`--wasm PATH`

Path to a `.wasm` binary to register as the class artifact. Mutually exclusive with `--aot`.

`--aot PATH`

Path to an AOT-compiled binary. Requires `--platform` and `--meta`.

`--meta PATH`

Path to the AOT metadata file.

`--platform PLATFORM`

Target platform for the AOT artifact. Accepted values:
- `riscv32imac` - Espressif RISC-V chips (ESP32-C5, ESP32-C6, ESP32-C61). Also accepted: `esp32c5`, `esp32_c5`, `esp32c6`, `esp32_c6`, `esp32c61`, `esp32_c61`.

`--force`

Overwrite the existing wasm binary if the class already has one registered.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Register a class from a wasm binary:

```bash
myrmic cells classes add my-cell --wasm ./my-cell.wasm
```

2. Register a class with an AOT artifact:

```bash
myrmic cells classes add my-cell --aot ./my-cell.aot --meta ./my-cell.meta --platform esp32c6
```
