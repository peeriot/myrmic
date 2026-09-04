# myrmic telemetry no-db-retention

## Name
`myrmic telemetry no-db-retention` - Disable telemetry database retention

## Synopsis
```
myrmic telemetry no-db-retention [OPTIONS]
```

## Description
Disables telemetry data expiry across all connected nodes. Applies to data inserted after this command runs - existing records are not affected. Use this when you need the full telemetry history.

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
