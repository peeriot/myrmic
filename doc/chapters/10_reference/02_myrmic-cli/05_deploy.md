# myrmic deploy

## Name
`myrmic deploy` - Deploy cells, application suites, or bridges

## Synopsis
```
myrmic deploy [OPTIONS] [PATH]
```

## Description
Deploy cells, application suites, or bridges to the swarm.

`PATH` specifies what to deploy:

- A `.wasm` binary - deploys the cell directly, without building.
- A `.nest` bundle - deploys a pre-built application suite directly.
- A `.yml` / `.yaml` bridge configuration file - deploys an MQTT or HTTP bridge.
- A cell crate directory or `Cargo.toml` - builds the cell and deploys it.
- A workspace directory or `Cargo.toml` - builds all member crates and deploys them.
- A `.yml` / `.yaml` application specification file - builds every cell class and deploys all instances atomically. If one instance fails, the entire deployment is rolled back.

When deploying from source (a crate, workspace, or app spec), the command builds automatically - no need to run `myrmic build` first.

If `PATH` is not provided, the current directory is used.

To learn about the bridge configuration file, see [Bridge Configuration](../01_configuration/05_bridge-configuration.md).

To learn about the application specification file, see [Cell and Application Configuration](../01_configuration/02_cell-and-application-configuration.md).

## Options
`--name SRN` (alias: `--srn`)

Sets the SRN (name) the deployment is registered under; the cell's SRI is derived from it. A UUID is rejected - the SRI cannot be set directly. How it is applied depends on what is being deployed:
- **Cell** - used as the cell's SRN. Defaults to the crate name when deploying from source, or the file name when deploying a `.wasm` binary.
- **`.nest` bundle** - ignored; names are read from the bundle metadata.
- **Bridge** - overrides the bridge name from the given configuration file.
- **Workspace** - used as a prefix for each crate (`<name><crate-name>`). Defaults to each crate's own name.
- **Application specification file** - ignored; names are taken from the specification file.

`--tag TAG` / `-t TAG`

Adds a placement requirement tag to the deployment. Can be specified multiple times.
Ignored for application suite deployments (use the `tags` field in the specification file) and `.nest` bundles (tags are in the bundle metadata).

`--platform PLATFORMS`

Build for the specified platforms before deploying. Multiple platforms may be specified as a comma-separated list. Defaults to `linux`. See [`myrmic platforms`](02_platforms.md) for the full list.

Every platform produces the same wasm binary; embedded platforms additionally produce an AOT-compiled artifact, which is registered alongside the wasm so the cell can be placed on a matching runtime. Use `--tag` to require such a placement.

Applies to cell crate and cell workspace deployments. Ignored for app suite deployments (set `platform` per class in the app spec instead), `.nest` bundles, and `.wasm` binaries, which are deployed as-is.

`--target TARGET`

Cargo target to build: `lib` or the name of a binary declared in `Cargo.toml`. When omitted, auto-selects:
- Exactly one binary - use it.
- No binaries, exactly one library - use the library.
- Otherwise - error and name one explicitly.

Applies to cell crate and cell workspace deployments. Ignored for app suite deployments - use `target` per class in the app spec instead.

`--init PAYLOAD`

Initialization arguments delivered to the cell's initialization handler on deploy. Encoded as JSON by default - a value that is not valid JSON is automatically wrapped as a JSON string. Only applies to single-cell (`.wasm` or crate) deploys - set `init` per instance in the app spec for application suites.

`--init-file PATH`

Path to a file that contains the initialization arguments passed to the cell's initialization handler. The file content format must match what the cell's initialization handler expects - JSON by default. Mutually exclusive with `--init`. Only applies to single-cell (`.wasm` or crate) deploys - set `init_file` per instance in the app spec for application suites.

`--policy POLICY`

Restart policy for the root cells this deployment creates. One of:
- `never` (the default) - a dead root stays dead.
- `on-error` (also spelled `onerror`) - restart on an abnormal exit: a crash, a spawn failure, node loss, or a non-zero stop code. A clean self-stop is terminal.
- `always` - restart on any exit. Only an operator terminate or a cascade teardown is terminal.

The crash-loop bounds keep their defaults (at most 5 restarts per 60s, 1s apart). To tune them, declare [`restart`](../01_configuration/02_cell-and-application-configuration.md#restart) per instance in the app spec instead.

Only roots are restarted - a cell spawned by another cell recovers through its parent's cell-lost handler, not this policy.

How it is applied depends on what is being deployed:
- **Cell**, **workspace**, **`.nest` bundle** - applied to every root the deployment creates.
- **Application specification file** - applied to every instance, replacing any `restart` the specification declares. Each instance whose declared policy is discarded is named in a warning.
- **Bridge** - ignored with a warning; bridges have no restart policy.

`-v` / `--verbose` - Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`
  Prints help information.

## Examples
1. Deploy a cell under a specific name:

```bash
myrmic deploy ./my-cell --name my-cell-name
```

2. Deploy a cell with placement tags:

```bash
myrmic deploy ./my-cell --tag my-tag-1 --tag my-tag-2
```

3. Deploy a `.wasm` binary:

```bash
myrmic deploy ./my-cell.wasm
```

4. Deploy an application suite:

```bash
myrmic deploy ./my-app.yml
```

5. Deploy a pre-built application suite:

```bash
myrmic deploy ./my-app/myrmic.nest
```

6. Deploy an MQTT or HTTP bridge:

```bash
myrmic deploy ./my-bridge.yml
```

7. Deploy a cell with init arguments:

```bash
myrmic deploy ./my-cell --init '{"greeting":"hello"}'
```

8. Deploy a cell with init arguments from a file:

```bash
myrmic deploy ./my-cell --init-file ./init
```

9. Deploy a cell built for an embedded platform:

```bash
myrmic deploy ./my-cell --platform esp32c6 --tag esp32c6
```

10. Deploy a cell that is restarted after a crash:

```bash
myrmic deploy ./my-cell --policy on-error
```
