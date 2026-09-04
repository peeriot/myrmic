# myrmic new

## Name
`myrmic new` - Create a new cell crate

## Synopsis
```
myrmic new [OPTIONS] [PATH]
```

## Description
Create a new cell crate at `PATH`. The cell crate is created with a `Cargo.toml` manifest, a `.gitignore`, and a minimal counter cell in `src/lib.rs` as a starting point.

The generated crate is pre-configured with the Myrmic Wasm SDK, pinned to the myrmic version in use.

## Options
`--name NAME`

Set the cell's crate name inside `Cargo.toml` file. Defaults to the directory name specified by `PATH`.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Create a new cell:

```bash
myrmic new my-cell
```
