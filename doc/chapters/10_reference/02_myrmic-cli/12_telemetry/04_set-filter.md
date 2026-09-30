# myrmic telemetry set-filter

## Name
`myrmic telemetry set-filter` - Change the log filter on all connected swarm nodes

## Synopsis
```
myrmic telemetry set-filter [OPTIONS] [FILTER]
```

## Description
Changes the active log filter on all connected swarm nodes without a restart. `FILTER` uses the [`tracing`](https://docs.rs/tracing) crate's [`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) format - level and target expressions, e.g. `info` or `debug,zenoh=off`.

`FILTER` **replaces** the node's whole filter; it is not merged into the one the node started with. A plain `zenoh=off` therefore also silences the two targets that report a node moving to a new address, which the shipped configurations carve out on purpose. Carry them with it:

```text
debug,zenoh=off,zenoh::net::runtime::interface_monitor=debug,zenoh::net::runtime::orchestrator=info
```

See [Addresses that change](../../../11_security.md#addresses-that-change) for what those targets report.

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Set the log level to info:

```bash
myrmic telemetry set-filter "info"
```

2. Set debug logging and silence the `zenoh` crate, apart from the targets that report an address change:

```bash
myrmic telemetry set-filter "debug,zenoh=off,zenoh::net::runtime::interface_monitor=debug,zenoh::net::runtime::orchestrator=info"
```
