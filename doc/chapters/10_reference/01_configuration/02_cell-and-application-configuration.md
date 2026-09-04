# Cell and Application Configuration
This page covers how to configure Cells and application suites during the build and the deployment process.

To read more about build and deploy commands, see [`myrmic build`](../02_myrmic-cli/03_build.md) and [`myrmic deploy`](../02_myrmic-cli/05_deploy.md).

## Single Cell
A Cell is configured in two stages, each covering different aspects: build time and deployment time.

1. During **deployment** using flags passed to the CLI deploy command - covers options like capability tags, name, and Cell SRI. See the [deploy command reference](../02_myrmic-cli/05_deploy.md) for details.
2. During the **build** process through the `Cargo.toml` file of the Cell's crate, using the `[package.metadata.myrmic]` section. This section is included in the Cell's manifest when scaffolded by the myrmic CLI. It controls the Cell's memory layout - heap, stack, and linear memory limits.

The section accepts four fields that define how much memory the Cell gets, and the values of these fields are compiled into the cell's Wasm binaries and can't be changed at runtime, the fields are the following:

- `heap_size` *(optional)* - Sets the amount of heap memory available to the cell during runtime in bytes. Can also be set through the `WASM_SDK_HEAP_SIZE` environment variable. Defaults to `8000`.
- `stack_size` *(optional)* - Sets the amount of stack memory available to the cell during runtime in bytes. Defaults to `32_768`.
- `initial_memory` *(optional)* - Sets the amount of initial linear memory allocated to the cell at startup in bytes. Defaults to `131_072`.
- `max_memory` *(optional)* - Sets the maximum amount of linear memory the Cell can use, in bytes. Defaults to `131_072`.

**Example:**
```toml
[package.metadata.myrmic]
heap_size = 32_000
stack_size = 8_192
initial_memory = 131_072
max_memory = 262_144
```

> **Note:** For workspaces there is no way to configure memory at the workspace level - only through the `Cargo.toml` of each member Cell.

## Application
An application groups multiple Cells that belong together into a single unit that is built and deployed together. It is defined through a **YAML application specification file** passed to the myrmic CLI build and deploy commands.

The file defines which Cells and bridges to build (`classes`) and which instances to deploy (`instances`).

The YAML application specification file accepts the following top-level keys:

### `name`
*(optional)* - Unique name used to identify the application in the swarm. Defaults to the spec file's parent directory name.

### `classes`
A list of buildable classes. Each entry has an `id` and is either a **cell** (built from a crate) or a **bridge** (described by a spec file). `build` and `spec` are mutually exclusive.

- `id` - **Required.** Identifier for the class, referenced from `instances`.
- `build` - Cell classes. Build configuration; a bare string is taken as the crate path. Omitting both `build` and `spec` builds the crate in the app-specs folder (`path: "."`) with automatic target selection.
  - `path` *(optional)* - Aliases: `source`, `directory`, `dir`. Path to the Cell's `Cargo.toml` or source directory. Defaults to `.`.
  - `target` *(optional)* - A single cargo target: `lib` or a target name. A class produces exactly one artifact, so at most one target may be named. Omit to auto-select (sole binary, else sole library).
  - `platforms` *(optional)* - One or a list of build platforms: `linux`, `esp32c5`, `esp32c6`, `esp32c61`. Defaults to `linux`.
- `spec` - Bridge classes. Aliases: `http`, `mqtt`, `cell`. Path to the bridge specification YAML file, relative to the app-specs file.

### `instances`
A list of instances to deploy. Each entry references exactly one entry from `classes`:

- `class` - References a cell `classes` entry by `id`.
- `bridge` - References a bridge `classes` entry by `id`.
- `sri` *(optional)* - SRI to deploy this instance under. Defaults to the referenced class id.
- `tags` *(optional)* - Capability tags the target runtime must advertise - controls which runtimes this Cell is placed on. Defaults to an empty list, meaning it deploys to any runtime.
- `init` *(optional)* - **Cell instances only.** Arguments delivered to the Cell's initialization handler on deploy. Encoded as JSON by default; a value that isn't valid JSON is sent as a JSON string.
- `init_file` *(optional)* - **Cell instances only.** Path to a file relative to the app-spec directory, whose raw bytes are delivered verbatim to the Cell's initialization handler.
- `restart` *(optional, work in progress)* - **Cell instances only.** Restart policy for the Cell this instance deploys. Either a bare policy name or a mapping that also tunes the crash-loop bounds - see [`restart`](#restart) below. Defaults to `never`.

> `class` and `bridge` are mutually exclusive, and the referenced class must be of the matching kind.
>
> `init` and `init_file` are mutually exclusive.
>
> `init`/`init_file`/`restart` apply only to cell instances. Bridges have no restart policy.
>
> It is possible to deploy one class under different SRIs by listing multiple instances that reference it.

#### `restart`

> **Work in progress**
> Restart policies are being developed as part of the Basic Cell stage and are not yet covered by the [guarantee contract](../../08_guarantees.md). A restart is not continuity: Cell State and Mailboxes are held as one copy by default, so a Cell restarted after Node loss does not resume from where it stopped. See the [roadmap](../../09_roadmap.md) for the stages that add replicated authority and data.

The policy name is one of:

- `never` (the default) - a dead Cell stays dead.
- `on-error` (also spelled `onerror`) - restart on an abnormal exit: a crash, a spawn failure, Node loss, or a non-zero stop code. A clean self-stop is terminal.
- `always` - restart on any exit. Only an operator terminate or a cascade teardown is terminal.

Written as a bare string, the crash-loop bounds keep their defaults. Written as a mapping, the bounds can be tuned:

- `type` - **Required.** The policy name.
- `max` *(optional)* - Maximum number of restarts within `window` before the Cell is left dead. Defaults to `5`.
- `window` *(optional)* - Length of the crash-loop window, as a human-readable duration (`30s`, `500ms`, `1m`). Defaults to `60s`.
- `delay` *(optional)* - Pause between an exit and the next restart, as a human-readable duration. Defaults to `1s`.

```yaml
instances:
  - class: my-cell-class-id-1
    restart: always

  - class: my-cell-class-id-2
    restart:
      type: on-error
      max: 3
      window: 30s
      delay: 2s
```

Passing `--policy` to [`myrmic deploy`](../02_myrmic-cli/05_deploy.md) replaces the `restart` declared by every instance in the file; each instance whose declared policy is discarded is named in a warning.

To read more about the bridge specification format and how to configure them, see [Bridge Configuration](./05_bridge-configuration.md).

**Examples:**
```yaml
name: my-app

classes:
  - id: my-cell-class-id-1
    build:
      path: ./my-cell-1
      platforms:
        - linux
        - esp32c6

  - id: my-cell-class-id-2
    build:
      path: ./my-cell-2
      platforms: esp32c6

  - id: my-mqtt-bridge
    spec: my-mqtt_bridge.yml

instances:
  - class: my-cell-class-id-1
    sri: my-cell-1
    tags: [my-tag-1]

  - class: my-cell-class-id-2
    sri: my-cell-2
    tags: [my-tag-2]

  - bridge: my-mqtt-bridge
    sri: bridge.mqtt
```

## See Also
- [Runtime Configuration](./01_runtime-configuration.md) - runtime configuration
- [Bridge Configuration](./05_bridge-configuration.md) - MQTT and HTTP bridge specification format
- [Myrmic CLI Reference](../02_myrmic-cli.md) - full reference for all CLI commands
- [`myrmic gateway`](../02_myrmic-cli/10_gateway.md) - how a cell gets served over HTTP
