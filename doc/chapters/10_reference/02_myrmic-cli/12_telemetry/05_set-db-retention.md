# myrmic telemetry set-db-retention

## Name
`myrmic telemetry set-db-retention` - Set the telemetry database retention period

## Synopsis
```
myrmic telemetry set-db-retention [OPTIONS] [RETENTION]
```

## Description
Sets how long telemetry data is kept in the swarm, across all connected nodes. Applies only to data inserted after this command runs - the already existing records are not affected. `RETENTION` accepts humantime duration string - e.g. `7d`, `1year 6months`.

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Set the retention period to one year and six months:

```bash
myrmic telemetry set-db-retention "1year 6months"
```
