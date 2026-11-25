# myrmic new

## Name
`myrmic new` - Scaffold a new cell or ESP32 firmware crate

## Synopsis
```
myrmic new [OPTIONS] <PATH>
```

## Description
Create a new crate at `PATH`, wired up against peeriot's `myrmic_sdk` and pinned to the myrmic
version in use. What is scaffolded depends on `--firmware`:

| Flags | Scaffolds |
|---|---|
| *(none)* | A **cell** crate — a `Cargo.toml`, a `.gitignore`, and a minimal counter cell in `src/lib.rs`. |
| `--firmware[=<chip>]` | An **ESP32 firmware** crate with the chip as its default feature, and that chip's `partitions.toml`. Build and flash it with [`myrmic build`](03_build.md) / `myrmic flash`. |

Signal Layer projects and modules are scaffolded by
[`myrmic signal-layer`](16_signal-layer/01_new.md) instead.

## Options
`-f`, `--firmware[=<CHIP>]`

Scaffold a firmware crate instead of a cell. `<CHIP>` is one of `esp32c5`, `esp32c6`, `esp32c61`, and
becomes the crate's default chip feature; the manifest itself targets the whole family (`firmware = "esp32"`).
The value requires the `=` form (`--firmware=esp32c6`); a bare `--firmware` defaults to `esp32c6`.

`--sdk <SDK>` (alias `--repo`)

Where the generated crate resolves the SDK crates from. Accepts a path to a local checkout (path
dependencies — no network) or a version/revision (a git or registry dependency). Defaults to the
revision the CLI was built from.

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

Scaffold an ESP32-C6 firmware:

```bash
myrmic new --firmware=esp32c6 my-fw
```

## See also
- [First Steps with the Signal Layer](../../04_tutorials/04_first-steps-signal-layer.md) - a
  tutorial that scaffolds a pipeline, a cell, and a firmware.
- [`myrmic signal-layer new`](16_signal-layer/01_new.md) - a Signal Layer pipeline project, on
  Linux or into an ESP32 firmware.
- [`myrmic build`](03_build.md), [`myrmic deploy`](05_deploy.md)
