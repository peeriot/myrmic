# myrmic telemetry traces

## Name
`myrmic telemetry traces` - Export swarm traces as JSON

## Synopsis
```
myrmic telemetry traces [OPTIONS]
```

## Description
Export OTel spans from the swarm's distributed telemetry database as JSON, viewable in [Perfetto](https://ui.perfetto.dev) or Chrome's `chrome://tracing`.

## Options
`-t` / `--trace-id TRACE_ID`

Filters output to spans belonging to a specific trace - for example, the trace ID printed by [`myrmic send`](../08_send.md).

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Export all traces:

```bash
myrmic telemetry traces
```

2. Export spans for a specific trace:

```bash
myrmic telemetry traces --trace-id 15f2af9ffee663762108dfec6d7de906
```
