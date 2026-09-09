# myrmic new

## Name
`myrmic new` - Scaffold a new cell, Signal Layer pipeline, or ESP32 firmware project

## Synopsis
```
myrmic new [OPTIONS] <PATH>
```

## Description
Create a new project at `PATH`, wired up against peeriot's `myrmic_sdk` and pinned to the myrmic
version in use. What is scaffolded depends on `--firmware` and `--pipeline`:

| Flags | Scaffolds |
|---|---|
| *(none)* | A **cell** crate — a `Cargo.toml`, a `.gitignore`, and a minimal counter cell in `src/lib.rs`. |
| `--pipeline` | A **Linux Signal Layer pipeline** project — `board.yml` + `pipeline.yml`, and a `build.rs` that generates the pipeline from them. A standalone tokio binary you run with `cargo run`. |
| `--firmware[=<chip>]` | An **ESP32 firmware** crate for the chip, with its `partitions.toml`. |
| `--firmware[=<chip>] --pipeline` | An **ESP32 firmware with a Signal Layer pipeline** — `board.yml` + `pipeline.yml` generated into the firmware image at build time. |

Every pipeline scaffold starts with a headless `sim-source` device, so it builds and runs with no
hardware attached. Build and flash a firmware with [`myrmic build`](03_build.md) /
[`myrmic flash`](../02_myrmic-cli.md); a Linux pipeline runs with `cargo run`.

## Options
`-f`, `--firmware[=<CHIP>]`

Scaffold a firmware crate instead of a cell. `<CHIP>` is one of `esp32c5`, `esp32c6`, `esp32c61`.
The value requires the `=` form (`--firmware=esp32c6`); a bare `--firmware` defaults to `esp32c6`.

`--pipeline`

Also scaffold a Signal Layer pipeline (`board.yml` + `pipeline.yml`). With `--firmware` the
pipeline is generated into the firmware image; on its own it scaffolds a standalone Linux pipeline
project.

`--sdk <SDK>` (alias `--repo`)

Where the generated project resolves the SDK and Signal Layer crates from. Accepts a path to a
local checkout (path dependencies — no network) or a version/revision (a git or registry
dependency). Defaults to the revision the CLI was built from.

`-n`, `--name <NAME>`

Set the crate name inside `Cargo.toml`. Defaults to the directory name from `PATH`.

`--timeout <TIMEOUT>`

How long to wait before giving up, e.g. `2s`, `500ms`, `1m30s`.

`-v` / `--verbose`

Verbose mode; repeat (`-vv`) for more detail.

`-h`, `--help`

Print help information.

## Examples

Create a cell:

```bash
myrmic new my-cell
```

Scaffold a Linux Signal Layer pipeline (build against a local checkout):

```bash
myrmic new --pipeline my-pipeline --sdk ~/myrmic
cd my-pipeline && cargo run
```

Scaffold an ESP32-C6 firmware:

```bash
myrmic new --firmware=esp32c6 my-fw
```

Scaffold an ESP32-C6 firmware with a Signal Layer pipeline:

```bash
myrmic new --firmware=esp32c6 --pipeline my-node --sdk ~/myrmic
```

## See also
- [First Steps with the Signal Layer](../../04_tutorials/03_first-steps-signal-layer.md) - a
  tutorial that scaffolds a pipeline, a cell, and a firmware.
- [`myrmic build`](03_build.md), [`myrmic deploy`](05_deploy.md)
- [Signal Layer reference](../04_signal-layer.md) - the `board.yml` and `pipeline.yml` formats.
