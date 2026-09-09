# Part 1 - The CLI

This is Part 1 of the [Observability](../06_observability.md) tutorial. You configure a runtime to keep its telemetry, produce some activity with the Quickstart's `counter` cell, and read logs, traces and metrics with the `myrmic telemetry` commands. Everything here works with the `myrmic` you already have.

---

## Step 1 - Configure the Runtime

By default a Myrmic runtime prints its logs to stdout and stores nothing. Telemetry is configured in the `telemetry` section of the runtime configuration file (YAML) - a top-level key, next to `execution`, `db` and `gateway`. In Terminal 2, in your `myrmic-quickstart/` directory, create `my-runtime.yml`:

```yaml
telemetry:
  # Turn on storage in the internal DB and keep records this long (humantime syntax).
  # Without it nothing is stored and the `myrmic telemetry` query commands show nothing.
  db_retention: "1h"

  logs:
    # Which log records are emitted - to stdout and to the DB.
    # The same filter decides which spans are recorded, so it also controls traces.
    # Syntax (tracing EnvFilter): <target>=<level> or just <level>, comma separated.
    #   "info"                    - info and above from every target (cells included)
    #   "info,zenoh=off"          - the same, without the transport library
    #   "debug,h2=warn,zenoh=off" - debug overall, quieter libraries
    # If omitted, the RUST_LOG environment variable is used instead.
    env_filter: "info"

    # Format of the log lines printed to stdout. No effect on what is stored.
    #   FULL    - timestamp + level + target + fields (default)
    #   COMPACT - shorter single-line output
    #   PRETTY  - human-friendly multi-line output, good for development
    #   JSON    - machine-readable JSON, good for log aggregators
    format: "FULL"
```

The [Runtime Configuration reference](../../10_reference/01_configuration/01_runtime-configuration.md#telemetry) lists the remaining keys; Part 2 adds three of them.

> **Why `info` and not `swarm=info,warn`?** A target filter such as `swarm=info,warn` keeps the runtime's own `swarm::…` lines and drops everything else at INFO. Cells log under the target `sorg_execution::wasm::host_functions::logging`, and the spans of your command handlers live outside the `swarm` crate too - so with that filter the DB receives the runtime's chatter but **no cell log lines and no traces**. Start with `info`; if the transport library is too noisy, use `info,zenoh=off`.

> **Note:** the runtime only checks the *top level* of the file. A misspelt key inside `telemetry` - `filter` instead of `env_filter`, say - is ignored without any message.

---

## Step 2 - Start the Runtime

In Terminal 1, from `myrmic-quickstart/`, start a runtime with that file and leave it running:

```bash
myrmic runtimes start my-runtime.yml
```

Wait for the readiness line:

```text
INFO  runtime "default" ready (<id>)
```

From now on every log record, span and metric the runtime and its cells emit is written to the internal DB. Nothing from before this point is stored - `db_retention` only applies to records emitted while it is set.

---

## Step 3 - Generate Some Activity

Back in Terminal 2. Deploying a cell produces its first log line (the `init` handler logs `starting`). To get traces you need a cell to handle a message - each command a cell processes produces a trace, and the CLI prints the trace ID of every command it sends.

```bash
# Deploy the Quickstart counter
myrmic deploy counter

# Send three commands - each one produces a trace and a log line in the cell
myrmic send counter increment
myrmic send counter increment
myrmic send counter increment
```

Each `send` prints the trace ID of the command it just sent:

```text
INFO  trace ID = b73cb3c74ef51f9e08eb767027963788
INFO  successfully sent command
```

Keep one of those IDs at hand - the next steps use it to follow a single command through logs and traces.

---

## Step 4 - Read Logs, Traces and Metrics

The `myrmic telemetry` subcommands query the internal DB directly. Each one has a page in the [CLI reference](../../10_reference/02_myrmic-cli.md); this step shows what they return for the activity of Step 3.

### Logs

```bash
myrmic telemetry logs
```

Shows the stored log records sorted by time, colour-coded by severity. Each row has the severity, a timestamp, and the message; a row that belongs to a trace shows the trace ID inline. Among the runtime's own lines you will find the counter's:

```text
INFO  [2026-09-08T22:26:07.676+02:00] | trace_id = 890a395a726e3e028f32f084585d870c | starting (id=Sri(5d883103-…))
INFO  [2026-09-08T22:26:07.695+02:00] | trace_id = b73cb3c74ef51f9e08eb767027963788 | Incremented count to 1 (sender=Sri(00000000-…))
INFO  [2026-09-08T22:26:07.707+02:00] | trace_id = 87850ecddbe587e05622fd76ea073b2f | Incremented count to 2 (sender=Sri(00000000-…))
INFO  [2026-09-08T22:26:07.721+02:00] | trace_id = f70b5a24c9cefdf6169cd129af82bf66 | Incremented count to 3 (sender=Sri(00000000-…))
```

The trace IDs are the ones `myrmic send` printed in Step 3. Pass one of them to see only the records of that command:

```bash
myrmic telemetry logs --trace-id b73cb3c74ef51f9e08eb767027963788
```

> **`DEBUG`/`TRACE` records are hidden by default.** `myrmic telemetry logs` reuses the CLI's own `-v`
> verbosity flag to decide which stored severities to print. `ERROR`, `WARN` and `INFO` always show;
> `DEBUG` needs `-v` and `TRACE` needs `-vv` - provided the runtime's filter let them through when they
> were recorded (Step 1, and [Step 5](#step-5---change-the-filter-and-the-retention-at-runtime) below):
>
> ```bash
> myrmic telemetry logs -v       # also DEBUG
> myrmic telemetry logs -vv      # also TRACE
> ```
>
> This only affects `logs`. `myrmic telemetry traces` always dumps every stored span.

### Traces

```bash
myrmic telemetry traces > trace.json
```

Writes all stored spans as a JSON array in the [Trace Event Format](https://docs.google.com/document/d/1CvAClvFfyA5R-PhYUmn5OOQtYMH4h6I0nSsKchNAySU/preview) (the Chrome trace format). Open https://ui.perfetto.dev/ and drop `trace.json` on it to browse the spans on a timeline. Each `increment` from Step 3 is one trace: a span named `increment` in the category `cell_task::message_handler`, plus a span for the cell's log line (category `wasm_log`) linked to it.

The export contains *every* recorded span, and a runtime records far more than your handlers - after a few minutes most of the file is runtime and transport spans. When you are after one command, export just its trace:

```bash
myrmic telemetry traces --trace-id b73cb3c74ef51f9e08eb767027963788 > one-trace.json
```

### Metrics

```bash
myrmic telemetry metrics
```

Prints the **latest snapshot** of every metric: name and value on one line, attributes indented below. Byte values are shown as human-readable sizes. Metrics are exported in intervals, so this prints **nothing during the first minute** after the runtime started - run it again after that. For the counter you will find, among some 50 metric families:

```text
cell_commands_processed  3
  host=cell_interaction
  cmd=increment
  kind=command
  pid=974858
  sri=5d883103-6382-5d16-9a0c-d70fd0565cb6
process.memory.usage  79.9 MiB
  host=c85a171750954bacbc9bf2f07caf77d4
  pid=974858
```

---

## Step 5 - Change the Filter and the Retention at Runtime

Both settings from Step 1 can be changed **without restarting any node**. The new value is broadcast to all connected nodes and takes effect within a few seconds.

### The log filter

```bash
# Everything at DEBUG, except the transport library
myrmic telemetry set-filter "debug,zenoh=off"

# Back to normal
myrmic telemetry set-filter "info"
```

Filter syntax follows the `tracing` crate's [`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) format, and the filter decides what is stored *and* which spans are recorded. Two things to know before you type `debug`:

- A bare `debug` floods the DB - the runtime's internal DEBUG lines arrive at hundreds of rows per second. Exclude the transport (`zenoh=off`) as above, or raise only the cells' level with `myrmic telemetry debug --level DEBUG` (Step 6), which restores the filter when it exits.
- `swarm=debug,warn` does **not** show you more of your cells. Cell log lines are emitted under the target `sorg_execution::wasm::host_functions::logging`, so a target filter on `swarm` leaves them - and their traces - out. If you need a target filter, include both: `swarm=info,sorg_execution=info,warn`.

### The DB retention

```bash
# Keep only the last 15 minutes of data
myrmic telemetry set-db-retention "15min"

# Keep 1 day and 30 minutes
myrmic telemetry set-db-retention "1day 30min"

# Stop storing telemetry in the DB (the default)
myrmic telemetry no-db-retention
```

Each command answers with `DB retention set to '…' on all connected nodes`. A value set this way lives in the running processes only: after a runtime restart the configuration file's `db_retention` applies again, or nothing if it has none. Only records emitted after a retention is set are kept.

The retention period is also the storage bound on the node: every log record and span the filter lets through is written to the runtime's database until it expires, and a runtime at `info` accumulates a lot within a day - at `debug`, hundreds of rows per second. On a device with little disk, keep the retention short, or turn it on only while you investigate.

Before you go on, make sure storage is on again:

```bash
myrmic telemetry set-db-retention "1h"
```

---

## Step 6 - The Live Debug Stream

The commands so far inspect data that has already been recorded. `myrmic telemetry debug` stays running and streams activity **as it happens**, until you press Ctrl-C or a `--timeout` elapses. Open Terminal 3 and start it:

```bash
myrmic telemetry debug --timeout 60s
```

The stream shows two things, in timestamp order: **events**, as an `EVENT=<name> payload=<payload>, trace_id=<id>` line, and the **log lines of cells**, as `[time] LEVEL message`. From Terminal 2, publish an event first, then send a command:

```bash
myrmic publish rain 20
myrmic send counter increment
```

Terminal 3 shows the event - no cell handles `rain`, an event is shown regardless - and then the counter's log line, the moment its handler runs:

```text
INFO  starting debug stream
[2026-09-09T07:17:05.173000000Z] EVENT=rain payload=20, trace_id=23f54ecc-65f2-6b5a-9e68-617d74c58b02
[2026-09-09T07:17:12.161237086Z] INFO Incremented count to 4 (sender=Sri(00000000-0000-0000-0000-000000000000))
INFO  debugging ends after timeout
```

> **Publish first.** In Myrmic 0.5.0 the stream prints no log lines until it has seen its first event - it uses the event's timestamp to know from where to read the log table. A stream that only ever sees `myrmic send` stays silent. Publishing any event, handled or not, unblocks it; every cell log line from then on is shown.

Unlike `myrmic telemetry logs`, the stream is not gated by `-v`: every severity a cell emits is shown. Payloads are printed as JSON if they parse as JSON, otherwise as a string, otherwise as raw hex bytes. The [reference page](../../10_reference/02_myrmic-cli/12_telemetry/07_debug.md) also lists a `COMMAND` item for commands sent to a cell; in Myrmic 0.5.0 that item does not appear - a command shows up through the log lines its handler emits, as above.

Useful flags:

```bash
# Only the log lines of one cell (SRI or SRN); events are not shown in this mode
myrmic telemetry debug --id counter --timeout 30s

# One JSON object per line, e.g. to pipe into jq
myrmic telemetry debug --json

# Raise the cells' log level to DEBUG for the duration of the stream, leaving the
# runtime's own targets alone; the previous filter is restored on exit
myrmic telemetry debug --level DEBUG
```

`--level` prints what it changed: `raised cell log level to 'DEBUG' (filter: 'info,swarm::embedded=DEBUG,sorg_execution::wasm::host_functions::logging=DEBUG')`, and `restored filter to 'info'` when the stream ends.

This is usually the fastest way to answer "what is my swarm doing right now" while you reproduce an issue, rather than reproducing it first and then digging through `logs` and `traces` afterwards.

---

## Troubleshooting

**The runtime does not start: `unable to parse configuration file […]: unknown field`**
: The file has a key the runtime does not know at the top level - for example `myrmic:` wrapped around `telemetry`. `telemetry` is itself a top-level key (Step 1).

**`myrmic telemetry logs` answers `No log records found within the database; Did you set a retention period?`**
: Nothing is stored by default. Set `db_retention` in the configuration file or run `myrmic telemetry set-db-retention "1h"`, then generate new activity - only records emitted after the retention is set are kept. A retention set with the command is lost when the runtime restarts; the configuration file's value is not.

**Logs are there, but no cell log lines, and `myrmic telemetry traces` prints `[]`**
: The filter is too narrow. A target filter such as `swarm=info,warn` keeps only the runtime's own crate; cells log under `sorg_execution::wasm::host_functions::logging` and the handler spans are outside `swarm` too, so neither is recorded. Use `info` (or `info,zenoh=off`).

**`myrmic telemetry metrics` prints nothing**
: Metrics are exported in intervals; the first snapshot appears about a minute after the runtime started. If it stays empty, the retention is not set (see above).

**A `DEBUG` or `TRACE` log I expect isn't showing up in `myrmic telemetry logs`**
: Two independent filters are in play. First, the runtime's `env_filter` (or `myrmic telemetry set-filter`) controls what is recorded at all - at `"info"`, `DEBUG`/`TRACE` records are never stored. Second, `myrmic telemetry logs` hides `DEBUG`/`TRACE` severities by default - pass `-v` (`DEBUG`) or `-vv` (`TRACE`) to the command itself, e.g. `myrmic telemetry logs -v --trace-id <id>`. `myrmic telemetry debug --level DEBUG` handles both at once for cell logs.

**Too much noise in logs**
: Use `myrmic telemetry set-filter "info,zenoh=off"` at runtime to quieten the transport library without restarting. Avoid a bare `debug` - it stores hundreds of rows per second.

**`myrmic telemetry debug` prints nothing, although `myrmic send` works and `myrmic telemetry logs` shows the lines**
: In 0.5.0 the stream needs one event before it prints any log line (Step 6). Run `myrmic publish rain 20` - or any other event - while the stream is running; the cell log lines of every command sent afterwards appear.

---

## What Have You Learned

- A runtime stores telemetry only while a retention period is set - `db_retention` in the configuration file, or `myrmic telemetry set-db-retention` at runtime. The command's value does not survive a restart; the file's does.
- The `telemetry` section is a top-level key of the runtime configuration. Its `logs.env_filter` decides what is recorded - log records *and* spans - so a filter that excludes the cells' target silently removes cell logs and traces.
- `myrmic send` prints a trace ID; `myrmic telemetry logs --trace-id` and `myrmic telemetry traces --trace-id` follow that one command through the swarm.
- `logs` hides `DEBUG`/`TRACE` unless you pass `-v`/`-vv`; `traces` exports everything in the Trace Event Format; `metrics` shows the latest snapshot and needs about a minute after start.
- `set-filter` and `set-db-retention` reach all connected nodes within a few seconds, without a restart.
- `myrmic telemetry debug` streams events and cell log lines live - after it has seen its first event; `--level` raises the cells' log level for the duration of the stream.

## Next Step

Everything you saw lives in the swarm's own database and is read with the CLI. In [Part 2 - The Grafana Stack](./02_the-grafana-stack.md) the same runtime exports the same signals to Loki, Tempo and Prometheus - after a rebuild of the CLI with OpenTelemetry support.
