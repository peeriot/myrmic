# myrmic telemetry metrics

## Name
`myrmic telemetry metrics` - Print the latest metric values from the swarm

## Synopsis
```
myrmic telemetry metrics [OPTIONS]
```

## Description
Print the latest value of every OpenTelemetry metric from the swarm's distributed telemetry database - cell metrics (commands processed, mailbox size) and runtime metrics (CPU, memory, disk).

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Print all current metric values:

```bash
myrmic telemetry metrics
```
