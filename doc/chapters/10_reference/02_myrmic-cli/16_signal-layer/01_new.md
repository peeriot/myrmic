# myrmic signal-layer new

## Name
`myrmic signal-layer new` - Scaffold a Signal Layer pipeline project

Aliases: `myrmic sl new`

## Synopsis
```
myrmic signal-layer new [OPTIONS] <PATH>
```

## Description
Create a pipeline project at `PATH`: a `board.yml` describing the hardware, a `pipeline.yml`
describing what runs on it, and a crate whose `build.rs` generates the pipeline from the two at
build time. Where the pipeline runs depends on `--firmware`:

| Flags | Scaffolds |
|---|---|
| *(none)* | A **Linux pipeline** project: a standalone tokio binary. Build it with [`myrmic build`](../03_build.md), or run it with `cargo run`. |
| `--firmware[=<chip>]` | An **ESP32 firmware** for the chip with the pipeline generated into its image, plus the chip's `partitions.toml`. Build and flash it with [`myrmic build`](../03_build.md) and `myrmic flash`. |

Every scaffold starts with a headless `sim-source` device, so it builds and runs with no hardware
attached. The crate depends on every driver and step shipped with myrmic, so any pipeline written
against them compiles; only the modules the pipeline uses end up in the binary. Modules of your own
come from [`signal-layer driver`](02_driver.md) and [`signal-layer step`](03_step.md).

## Options
`-f`, `--firmware[=<CHIP>]`

Generate the pipeline into an ESP32 firmware for `<CHIP>` (`esp32c5`, `esp32c6`, `esp32c61`)
instead of a Linux pipeline project. The value requires the `=` form (`--firmware=esp32c5`); a bare
`--firmware` defaults to `esp32c6`.

`--sdk <SDK>` (alias `--repo`)

Where the generated project resolves the myrmic and Signal Layer crates from. Accepts a path to a
local checkout (path dependencies, no network) or a version/revision (a registry or git
dependency). Defaults to the revision the CLI was built from.

`-n`, `--name <NAME>`

Set the crate name inside `Cargo.toml`. Defaults to the directory name from `PATH`.

`-v` / `--verbose`

Verbose mode; repeat (`-vv`) for more detail.

`-h`, `--help`

Print help information.

## Examples

Scaffold a Linux pipeline against a local checkout and run it:

```bash
myrmic signal-layer new my-pipeline --sdk ~/myrmic
cd my-pipeline && cargo run
```

Scaffold an ESP32-C6 firmware with the pipeline in its image, then flash it:

```bash
myrmic sl new --firmware=esp32c6 my-node --sdk ~/myrmic
myrmic flash my-node --monitor
```

## See also
- [First Steps with the Signal Layer](../../../04_tutorials/04_first-steps-signal-layer.md) - a
  tutorial that scaffolds a pipeline, a cell and a firmware.
- [Describe your hardware](../../../05_guides/11_signal-layer/01_describe-your-hardware.md) and
  [Design your pipeline](../../../05_guides/11_signal-layer/02_design-your-pipeline.md) - what to
  put in the two YAML files.
- [`signal-layer driver`](02_driver.md), [`signal-layer step`](03_step.md) - modules of your own.
- [Signal Layer reference](../../04_signal-layer.md) - the `board.yml` and `pipeline.yml` formats.
