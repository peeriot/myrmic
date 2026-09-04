# myrmic telemetry debug

## Name
`myrmic telemetry debug` - Stream and print live activity from the swarm

## Synopsis
```
myrmic telemetry debug [OPTIONS]
```

## Description
Connects to the swarm and streams live debug information about commands, events, and log records from the swarm cells.

Records are printed as they arrive, sorted by timestamp.

Use it during development to get live visibility into what is happening inside the swarm.

Each record is one of three types:

- **Event** - it holds the event event name, a payload, and a trace ID.
- **Command** - it holds the command name, the receiving cell identifier , a payload, and a trace ID.
- **Log** - it hold the log level and message.

Command and event payloads are decoded as JSON if possible, then as a string, then as raw bytes.

## Options
`--id SRI/SRN`

Switch to cell view - streams commands and logs for the given cell only, identified by its SRI (UUID) or SRN name. Events are not shown because they are global broadcasts not addressed to any specific cell.

`--timeout DURATION`

Stop streaming after the given duration, accepts humantime duration string - e.g. `30s`, `5min`, `1h`. When omitted the stream runs until stopped by the user.

`--json`

Print each debug record as a JSON object. Useful for programmatic processing with tools like [jq](https://jqlang.org).

`-v` / `--verbose`

Global flag. Controls the CLI's own output verbosity, not the debug stream.

`-h`, `--help`

Prints help information.

## Examples
1. Stream events and logs from all cells:

```bash
myrmic telemetry debug
```

2. Stream commands and logs for a specific cell, stop after 30 seconds, pipe into jq:

```bash
myrmic telemetry debug --id asset.object.0 --timeout 30s --json | jq .
```
