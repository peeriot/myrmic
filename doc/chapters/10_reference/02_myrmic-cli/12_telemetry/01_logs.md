# myrmic telemetry logs

## Name
`myrmic telemetry logs` - Print log records from the swarm

## Synopsis
```
myrmic telemetry logs [OPTIONS]
```

## Description
Print log records from the swarm's distributed telemetry database. Logs are emitted by cells and swarm nodes - each record includes a timestamp, severity level, and message.

## Options
`-t` / `--trace-id TRACE_ID`

Filters output to records belonging to a specific trace - for example, the trace ID printed by [`myrmic send`](../08_send.md).

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Print all log records from the swarm:

```bash
myrmic telemetry logs
```

2. Filter logs to a specific trace:

```bash
myrmic telemetry logs --trace-id 15f2af9ffee663762108dfec6d7de906
```
