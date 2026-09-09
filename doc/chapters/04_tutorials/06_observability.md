# Observability

In this tutorial you learn to see what a Myrmic swarm is doing: which log lines a cell wrote, how a command travelled through the system, and how much work and memory a runtime spends. You do it twice - first with nothing but the Myrmic CLI, then with a Grafana stack that receives the same data over OpenTelemetry.

The application under observation is deliberately small: the `counter` cell from the [Quickstart](../01_quickstart.md). It is enough to produce every kind of signal, and it keeps the attention on the tooling rather than on the cells.

## What is Observability and Why Does it Matter?

Observability answers the question: *"What is my swarm actually doing right now?"*

A running swarm is a distributed system - multiple nodes communicating over a P2P layer. Without observability you are flying blind: when something goes wrong (a cell stalls, a message loop spikes latency, memory climbs) there is no way to diagnose it from the outside.

Myrmic exposes three complementary signals:

| Signal | What it tells you | Part 1 (CLI) | Part 2 (Grafana) |
|--------|-------------------|---|---|
| **Logs** | Timestamped text events from every node and every cell | `myrmic telemetry logs` | Loki |
| **Traces** | Distributed call trees showing how work flows through cells | `myrmic telemetry traces` | Tempo |
| **Metrics** | Numerical counters and gauges (memory, message counts, …) | `myrmic telemetry metrics` | Prometheus |

Each signal can go to one or both destinations:

- **Internal DB** - every runtime stores its telemetry in the swarm's distributed database, and the `myrmic telemetry` CLI queries it from any node without extra infrastructure. Storage is **off by default**: nothing is written until a retention period is set. Every `myrmic` binary has this.
- **OpenTelemetry (OTel) export** - a runtime pushes its own signals to an OTel collector, which forwards them to tools such as Grafana. This needs a `myrmic` binary built with the `open-telemetry` feature and an endpoint configured per signal.

Both destinations receive everything the runtime's log filter lets through, and that is a lot of data on a swarm that runs for days. Part 1 shows how the retention period bounds what is kept on the node; Part 2 how the filter bounds what leaves it.

## How the Tutorial is Organised

**Part 1** works with the `myrmic` you already have - the release package or a plain source build - and covers everything the CLI offers: the configuration that turns storage on, the `myrmic telemetry` commands for logs, traces and metrics, changing the log filter and the retention at runtime, and the live debug stream.

**Part 2** builds on Part 1. The configuration file grows by three lines, the runtime is restarted with it, and the trace IDs you learned to follow in the CLI are the ones you look up in Grafana. Part 2 starts with a **rebuild of the CLI**: the released packages and the default source build do not contain the OTel exporter. Building takes several minutes - if you plan to continue to Part 2, you can start the build described at the beginning of it while working through Part 1.

Terminals used throughout:

| Terminal | Runs |
|---|---|
| Terminal 1 | the Myrmic runtime |
| Terminal 2 | your working shell: deploy, send, `myrmic telemetry …` |
| Terminal 3 | `myrmic telemetry debug` - the live stream (Part 1, Step 6) |

## Prerequisites

- Completed the [Quickstart](../01_quickstart.md). The tutorial reuses its `counter` cell: the `myrmic-quickstart/` directory with the built `counter` crate should still exist. If not, run `myrmic new counter` and `myrmic build counter` again.
- No runtime running - stop any leftover with `myrmic runtimes stop`. Part 1 starts one from a configuration file.
- *Part 2 only:* the Myrmic repository cloned locally, the Rust toolchain via [rustup](https://rustup.rs/), the packages listed under [Install from source](../01_quickstart/01_installation.md#install-from-source), and Docker with Docker Compose.

## Tutorial Parts

1. [The CLI](./06_observability/01_the-cli.md) - turn telemetry storage on, generate activity with the counter, and inspect logs, traces and metrics from the command line - **learning how the runtime records telemetry and how the `myrmic telemetry` commands read it.**
2. [The Grafana Stack](./06_observability/02_the-grafana-stack.md) - rebuild the CLI with OpenTelemetry support, start the Grafana stack, and export the same signals to it - **learning how OTel export is configured per runtime and how to find your data in Loki, Tempo and Prometheus.**
