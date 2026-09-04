# myrmic build

## Name
`myrmic build` - build a cell, workspace, or application suite

## Synopsis
```
myrmic build [OPTIONS] [PATH]
```

## Description
Build a cell crate, a workspace of cells, or an application suite from source.

`PATH` specifies what to build:

- A cell crate directory - compiles the cell to a `.wasm` binary in the crate's `target` directory.
- A cell crate `Cargo.toml` - same as above, specified explicitly.
- A workspace directory - compiles all member crates to `.wasm` binaries in the workspace's `target` directory.
- A workspace `Cargo.toml` - same as above, specified explicitly.
- A `.yml` / `.yaml` application specification file - builds every cell class defined in the specification and bundles the compiled binaries and application metadata into a `.nest` bundle placed in the working directory.

If `PATH` is not provided, the current directory is used.

All cell builds run in release mode using `cargo +nightly` internally. Set the `CARGO` environment variable to use a specific `cargo` binary.

To learn about the application specification file, see [Cell and Application Configuration](../01_configuration/02_cell-and-application-configuration.md).

## Options
`--platform PLATFORMS`

Build for the specified platforms. Multiple platforms may be specified as a comma-separated list. Defaults to `linux`. See [`myrmic platforms`](02_platforms.md) for the full list.

Ignored for application suite builds (set `platform` per cell class in the application specification instead).

`--target TARGET`

Cargo target to build, accepts `lib` or the name of a binary declared in `Cargo.toml`. When omitted, auto-selects:
- Exactly one binary - use it.
- No binaries, exactly one library - use the library.
- Otherwise - returns an error.

Applies to cell crate and cell workspace builds. Ignored for app suite builds - use `target` per class in the app spec instead.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples

1. Build a cell:

```bash
myrmic build ./my-cell
```

2. Build for multiple platforms:

```bash
myrmic build --platform linux,esp32c6
```

3. Build a workspace (all member crates):

```bash
myrmic build ./my-workspace
```

4. Build an application suite from a specification file:

```bash
myrmic build ./my-app/app.yml
```

5. Select a specific cargo target:

```toml
[lib]
name = "my-cell"

[[bin]]
name = "server"
```

```bash
myrmic build --target lib        # build the library target
myrmic build --target server     # build the binary named "server"
```
