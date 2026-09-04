# Runtime Configuration

A Myrmic runtime can be configured in two ways:
- By passing a YAML file at startup.
- Through CLI flags when running the start command. This covers a reduced set of options compared to the YAML file.

Both options are applied at startup with `myrmic runtimes start`. See [myrmic runtimes start](../02_myrmic-cli/04_runtimes/01_start.md) for details and examples.

When starting a runtime without a config file or CLI flags, it starts with built-in defaults. A restart is required to apply any configuration change.

This page covers configuration through the YAML file. It is structured as top-level sections, each controlling a specific aspect of the runtime.

## `zenoh` *(advanced)*

Since Myrmic uses Zenoh as its transport layer. Configure it directly under the `zenoh` key - mode, peer discovery, transport, and connectivity. For all available fields, see the [Zenoh default config reference](https://github.com/eclipse-zenoh/zenoh/blob/main/DEFAULT_CONFIG.json5).

## `execution`

Configures the execution behavior.

- `mailbox_poll_interval_ms` *(optional)* - How often the runtime checks for new inbound messages, in milliseconds. Defaults to `5000`.
- `mailbox_batch_size` *(optional)* - Number of event rows fetched per mailbox poll interval. Defaults to `8`.
- `tags` *(optional)* - Capability tags this runtime advertises. Controls which Cells are placed on this runtime. Tags set here are combined with any additional tags passed via the CLI at startup. Defaults to none.
- `max_fuel_per_handler` *(optional)* - Compute budget the runtime grants to each handler invocation - limits how many wasm instructions a single handler invocation may execute before it is interrupted. Every invocation gets its own fresh budget, independent of the Cell and of previous invocations. See [External Links](#external-links) for the underlying wasmtime fuel mechanism. Defaults to unlimited.
- `fuel_yield_interval` *(optional)* - Number of fuel units a Cell consumes before yielding to the async scheduler, letting other tasks run. See [External Links](#external-links) for the underlying wasmtime fuel mechanism. Defaults to `1000`.
- `init_timeout_secs` *(optional)* - How long to wait for a Cell to initialize, in seconds. If exceeded, initialization fails and the entire deployment is rolled back. Defaults to `10.0`.

## `db`

Configures storage behavior.

- `directory` *(optional)* - Path to the directory where data is stored on disk. If omitted, data is stored in-memory and lost on restart.
- `gc_interval` *(optional)* - Garbage collection scan interval. Accepts [humantime](https://docs.rs/humantime/latest/humantime/) duration strings (e.g. `"100ms"`, `"30s"`, `"1min"`). Defaults to `"60s"`.
- `tx_idle_timeout` *(optional)* - How long an RPC transaction may sit unused before the store rolls it back. Accepts [humantime](https://docs.rs/humantime/latest/humantime/) duration strings. Defaults to `"5min"`.
- `load_from` *(optional)* - Loads files from disk into the database as blobs at startup. Each entry accepts:
  - `path` - **Required.** File or directory to read. If a directory, all files inside are loaded.
  - `scope` *(optional)* - The scope the files are stored under, in `namespace/database/schema` format. Defaults to `p/p/d`.
  - `prefix` *(optional)* - Prepended to each filename to form the database key. Must start and end with `/`. Defaults to `/`. e.g. a file named `token` with `prefix: "/secrets/"` is stored at key `/secrets/token`.
  - `max_depth` *(optional)* - When `path` is a directory, controls how deep to recurse. By default all files at any depth are loaded.

## `gateway`

When this section is present, the runtime also runs an embedded gateway - it starts and stops together with the runtime - providing an entry point for external clients to the swarm, to send commands and publish events over HTTP and WebSocket, and serving static web assets.

- `port` *(optional)* - TCP port the embedded gateway listens on. Defaults to `8080`.

To run the gateway as its own process independently of a runtime, use [`myrmic gateway`](../02_myrmic-cli/10_gateway.md) instead.

## `orchestration`

Configures the self-organization behavior.

- `init_timeout_secs` *(optional)* - How long the Self Organization Layer waits for a runtime to confirm a deployment, in seconds. If exceeded, the entire deployment is rolled back. Defaults to `15.0`.

## `telemetry`

Configures where logs go, how they are formatted, and whether telemetry is exported to an OpenTelemetry collector.

- `db_retention` *(optional)* - How long telemetry data is retained in the database. Accepts [humantime](https://docs.rs/humantime/latest/humantime/) duration strings (e.g. `"1h"`, `"7d"`). If omitted, data is kept indefinitely.
- `logs` *(optional)*
  - `format` *(optional)* - Log line format printed to stdout. Accepts `FULL`, `COMPACT`, `PRETTY`, or `JSON`. Defaults to `FULL`. See the [tracing-subscriber format reference](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/index.html).
  - `env_filter` *(optional)* - Controls which logs are printed. Can be changed at runtime without a restart - see [myrmic telemetry set-filter](../02_myrmic-cli/12_telemetry/04_set-filter.md). Uses [EnvFilter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) syntax: `target=level` or just `level` (e.g. `"info"`, `"swarm=debug,warn"`). If not set, the `RUST_LOG` environment variable is used instead.
  - `otel_endpoint` *(optional)* - OpenTelemetry endpoint for log export. If omitted, logs are not exported.
- `metrics` *(optional)*
  - `otel_endpoint` *(optional)* - OpenTelemetry endpoint for metrics export. If omitted, metrics are not exported.
- `traces` *(optional)*
  - `otel_endpoint` *(optional)* - OpenTelemetry endpoint for trace export. If omitted, traces are not exported.


> If a high level section is omitted, its default configuration is used.

## Example

```yaml
execution:
  mailbox_poll_interval_ms: 200
  mailbox_batch_size: 16
  tags: [my-flag-1, my-flag-2, my-flag-3]

db:
  directory: ./my-db-data
  load_from:
    - path: ./secrets
      scope: "p/secrets/tokens"
      prefix: "/secrets/"

gateway:
  port: 8080

telemetry:
  logs:
    format: JSON
    env_filter: "info,swarm=debug"
    otel_endpoint: "http://localhost:4317"
  metrics:
    otel_endpoint: "http://localhost:4317"
  traces:
    otel_endpoint: "http://localhost:4317"
```

## See Also

- [Myrmic CLI Reference](../02_myrmic-cli.md) - full reference for all CLI commands
- [Cell and Application Configuration](./02_cell-and-application-configuration.md) - Cells and Applications Configuration

## External Links

- [wasmtime `Store::set_fuel`](https://docs.rs/wasmtime/43.0.2/wasmtime/struct.Store.html#method.set_fuel) - how a fuel budget is set on a wasm store
- [wasmtime `Store::fuel_async_yield_interval`](https://docs.rs/wasmtime/43.0.2/wasmtime/struct.Store.html#method.fuel_async_yield_interval) - yielding to the async scheduler at a fuel interval
- [wasmtime deterministic fuel](https://docs.wasmtime.dev/examples-interrupting-wasm.html#deterministic-fuel) - background on interrupting wasm execution with fuel
