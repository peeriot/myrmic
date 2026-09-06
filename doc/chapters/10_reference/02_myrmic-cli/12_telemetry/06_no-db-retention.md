# myrmic telemetry no-db-retention

## Name
`myrmic telemetry no-db-retention` - Disable telemetry database retention

## Synopsis
```
myrmic telemetry no-db-retention [OPTIONS]
```

## Description
Stops writing telemetry to the swarm's internal DB across all connected nodes - this is the default state of a runtime. Applies to data emitted after this command runs - existing records are not affected. Use this when the swarm should not spend storage on telemetry; to record again, run [`myrmic telemetry set-db-retention`](./05_set-db-retention.md).

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Disable automatic data expiry:

```bash
myrmic telemetry no-db-retention
```
