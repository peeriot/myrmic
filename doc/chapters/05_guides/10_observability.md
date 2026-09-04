# Observability

In complex use cases, Myrmic applications span hundreds of nodes - cells are distributed across runtimes, extensively communicating with each other, processing data, reading from hardware, and writing to storage. All of this happens concurrently across the swarm - but what if something goes wrong? What if you want to understand what is happening inside at any given moment? Observability is a critical part of running a swarm at this scale - it gives you the tools to answer exactly those questions. This guide covers what Myrmic offers out of the box to give you that visibility. For a hands-on walkthrough, see the [Observability tutorial](../04_tutorials/06_observability.md).

## Check what's running

The starting point that makes sense before diving into any complex investigation is to check what is running - whether runtimes or cells.

The Myrmic CLI provides dedicated commands for this:

### List local runtimes

To inspect runtimes started on a machine, `myrmic runtimes list` shows each one and its current status. See [`myrmic runtimes list` reference](../10_reference/02_myrmic-cli/04_runtimes/02_list.md) for synopsis, options and examples.

### List swarm nodes

To inspect all runtimes connected across the swarm network, `myrmic network status` lists each node with its identifier, role, and swarm topology. See [`myrmic network status` reference](../10_reference/02_myrmic-cli/07_network/01_status.md) for synopsis, options and examples.

### List running cells

To inspect deployed cells or the status of a specific cell, `myrmic cells status` lists all deployed cells - each showing its identifier, type, and the runtime it runs on. See [`myrmic cells status` reference](../10_reference/02_myrmic-cli/06_cells/01_status.md) for synopsis, options and examples.

## Logs

When something within your distributed Myrmic application goes wrong, the first thing that comes to mind is to check the logs. They reveal what is happening inside the runtime process - every action, large or small - and what your cells are doing at the same time.

### How cells emit logs

To emit logs from within a cell, the Myrmic SDK provides five dedicated logging macros - one for each severity level:

```rust
use myrmic_sdk::{trace, debug, info, warn, error};

#[myrmic_sdk::cmd]
fn on_start(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    let _ = trace!("on_start: entering handler").ok();
    let _ = debug!("on_start: handler began").ok();
    let _ = info!("cell started").ok();
    let _ = warn!("upstream cell not yet available").ok();
    let _ = error!("failed to initialize").ok();

    Ok(())
}
```

These macros use [`format!`](https://doc.rust-lang.org/std/macro.format.html) internally - use them the same way.

### View logs

The CLI provides `myrmic telemetry logs` to inspect log entries from all connected runtimes and their cells. Log entries can be narrowed to a specific trace - traces are covered in the next section. See [`myrmic telemetry logs` reference](../10_reference/02_myrmic-cli/12_telemetry/01_logs.md) for synopsis, options and examples.

### Log verbosity

Log verbosity controls how much detail the runtime process and your cells emit. When debugging, you may want more detail to understand what is happening internally. When things are too noisy, you can reduce verbosity for specific components while keeping others at a useful level.

There are two ways to control verbosity:

- **At startup** - configure the log filter by setting `telemetry.logs.env_filter` in the runtime configuration. See [runtime configuration reference](../10_reference/01_configuration/01_runtime-configuration.md) for details.
- **At runtime** - the Myrmic CLI provides `myrmic telemetry set-filter` to apply a new filter across all connected runtimes without a restart. See [`myrmic telemetry set-filter` reference](../10_reference/02_myrmic-cli/12_telemetry/04_set-filter.md) for synopsis, options and examples.

## Traces

Logs show what happened on a single runtime and its cells. But in Myrmic, a single operation can involve many cells across the swarm - for example, a command that arrives at one cell may trigger calls to others. Traces capture that journey: every cell involved records a span, and the context travels with each message, linking them into a chain.

The Myrmic CLI provides `myrmic telemetry traces` to inspect trace spans - filtering by trace ID narrows results to a specific operation. The same ID can also be passed to `myrmic telemetry logs` to cross-reference related log lines. The output is in the [Trace Event Format](https://docs.google.com/document/d/1CvAClvFfyA5R-PhYUmn5OOQtYMH4h6I0nSsKchNAySU/preview) - a widely supported JSON format readable by any compatible trace viewer, such as [Perfetto](https://ui.perfetto.dev/) or chrome://tracing. See [`myrmic telemetry traces` reference](../10_reference/02_myrmic-cli/12_telemetry/02_traces.md) for synopsis, options and examples.

## Metrics

Metrics give you a snapshot of how the swarm is performing. They cover two things: how the runtime process is doing (CPU, memory, disk I/O) and how your cells are behaving (mailbox depth, commands processed, events processed).

The Myrmic CLI provides `myrmic telemetry metrics` to inspect current metric values. See [`myrmic telemetry metrics` reference](../10_reference/02_myrmic-cli/12_telemetry/03_metrics.md) for synopsis, options and examples.

## Live debugging

So far, all three signals - logs, traces, and metrics - are stored, past telemetry data. Good for history, but sometimes you need to see what is happening across the swarm right now. The Myrmic CLI provides `myrmic telemetry debug` to do exactly that: it reports activity from all connected runtimes and their cells - commands dispatched, events fired, and cell logs emitted - in real time as they occur. See [`myrmic telemetry debug` reference](../10_reference/02_myrmic-cli/12_telemetry/07_debug.md) for synopsis, options and examples.

## Telemetry retention

Telemetry data is stored in the data layer - logs, traces, and metrics each in their own dedicated table. With no limit set, they accumulate indefinitely - on long-running deployments, that is a storage concern.

There are two ways to set a retention period:

- **At startup** - set `telemetry.db_retention` in the runtime configuration. This applies to that specific runtime only. See [runtime configuration reference](../10_reference/01_configuration/01_runtime-configuration.md) for details.
- **At runtime** - the Myrmic CLI provides `myrmic telemetry set-db-retention` to apply a retention period across all connected runtimes, and `myrmic telemetry no-db-retention` to clear it. See [`myrmic telemetry set-db-retention` reference](../10_reference/02_myrmic-cli/12_telemetry/05_set-db-retention.md) and [`myrmic telemetry no-db-retention` reference](../10_reference/02_myrmic-cli/12_telemetry/06_no-db-retention.md) for synopsis, options and examples.

## Route telemetry to an OpenTelemetry collector

The CLI commands are useful for quick inspection and short debugging sessions. For anything more sustained - long-term storage, alerting, dashboards - you need an external system. Myrmic supports exporting all three signals to any OpenTelemetry-compatible collector, which can route them to tools like Grafana, Jaeger, or Prometheus.

To enable it, configure `otel_endpoint` for each - logs, traces, and metrics - under `telemetry` in the runtime configuration. See [runtime configuration reference](../10_reference/01_configuration/01_runtime-configuration.md) for details.

Once configured, each runtime pushes its own telemetry to the collector automatically as it is generated. Configure `otel_endpoint` on every runtime you want data from - runtimes without it set will not export.

> **Note:** Exporting to an external OpenTelemetry collector requires Myrmic to be built with the `open-telemetry` feature.

## See also

- [Observability tutorial](../04_tutorials/06_observability.md)
- [Myrmic CLI reference](../10_reference/02_myrmic-cli.md)
- [Runtime configuration reference](../10_reference/01_configuration/01_runtime-configuration.md)

## Related SDK reference

- [Logging](../10_reference/03_myrmic-sdk/10_logging.md)
- [Errors](../10_reference/03_myrmic-sdk/11_error-model.md)
- [Cell monitoring](../10_reference/03_myrmic-sdk/04_cell-lifecycle/03_cell-monitoring.md)
