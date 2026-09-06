# myrmic telemetry set-db-retention

## Name
`myrmic telemetry set-db-retention` - Set the telemetry database retention period

## Synopsis
```
myrmic telemetry set-db-retention [OPTIONS] [RETENTION]
```

## Description
Turns on telemetry storage in the swarm's internal DB and sets how long records are kept, across all connected nodes. Without a retention period (the default) nothing is written to the DB, so this command - or `db_retention` in the runtime configuration - is required before [`myrmic telemetry logs`](./01_logs.md), `traces`, or `metrics` show anything. Applies only to data inserted after this command runs - the already existing records are not affected. The value is not persisted across a runtime restart. `RETENTION` accepts humantime duration string - e.g. `7d`, `1year 6months`.

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
