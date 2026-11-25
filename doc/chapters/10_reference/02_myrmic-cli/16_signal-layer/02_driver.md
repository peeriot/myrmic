# myrmic signal-layer driver

## Name
`myrmic signal-layer driver` - Scaffold a Signal Layer driver module

Aliases: `myrmic sl driver`

## Synopsis
```
myrmic signal-layer driver [OPTIONS] --bus <BUS> <ID>
```

## Description
Create a driver module: the crate that talks to a device on a bus and produces the taps a pipeline
reads. It is created at `<registry-dir>/drivers/<ID>` as the crate `<ID>-driver`, with a
`descriptor.yaml` declaring the driver's configuration and taps, a `src/lib.rs` holding the driver
skeleton for the bus, and a stubbed test suite that runs with `cargo test`.

`ID` is the module id (`bme280`), not a path; where the crate goes is `--registry-dir`. A module
with the id of a driver shipped with myrmic overlays the shipped one wherever its registry is
included.

A pipeline uses the module by pointing its `build.rs` at the registry directory
(`.include("../my-modules")`) and adding the crate to its `Cargo.toml`; the command prints the
dependency line to add.

## Options
`--bus <BUS>`

The bus the driver talks over: `i2c` or `spi`. Picks the driver template. Required.

`--registry-dir <DIR>`

Base directory the module is placed under, as `drivers/<ID>`. Defaults to the current directory.

`-v` / `--verbose`

Verbose mode; repeat (`-vv`) for more detail.

`-h`, `--help`

Print help information.

## Examples

Scaffold an I2C driver into a module registry and run its stubbed tests:

```bash
myrmic signal-layer driver bme280 --bus i2c --registry-dir ~/my-modules
cd ~/my-modules/drivers/bme280 && cargo test
```

## See also
- [Write your own driver](../../../05_guides/11_signal-layer/07_write-your-own-driver.md) - what
  to fill in after scaffolding.
- [Drivers](../../04_signal-layer/03_drivers.md) - the descriptor format and the driver contract.
- [`signal-layer new`](01_new.md) - the pipeline project that uses the module.
