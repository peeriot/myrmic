---
sidebar_label: Myrmic CLI
---

# Myrmic CLI

The Myrmic CLI is the command-line tool for creating, building, deploying, and interacting with Myrmic cells and applications.

## Synopsis

```
myrmic [OPTIONS] COMMAND [ARGS]
myrmic --help
myrmic --version
```

## Environment Variables

The following environment variables control the behavior of the `myrmic` command-line tool.

| Variable | Description | Used by |
|---|---|---|
| `CARGO` | Path to the `cargo` executable. Defaults to `cargo` on `PATH`. | [`build`](02_myrmic-cli/03_build.md), [`deploy`](02_myrmic-cli/05_deploy.md) |
| `XDG_RUNTIME_DIR` | Controls where PID files are stored. | [`runtimes start`](02_myrmic-cli/04_runtimes/01_start.md), [`runtimes list`](02_myrmic-cli/04_runtimes/02_list.md), [`runtimes delete`](02_myrmic-cli/04_runtimes/03_delete.md) |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` | S3 credentials. | [`database export`](02_myrmic-cli/13_database/01_export.md), [`database import`](02_myrmic-cli/13_database/02_import.md) |
| `AWS_REGION` / `AWS_DEFAULT_REGION` | AWS region for S3. Falls back to `us-east-1`. | [`database export`](02_myrmic-cli/13_database/01_export.md), [`database import`](02_myrmic-cli/13_database/02_import.md) |

## Commands

Below is the full list of `myrmic` commands, each with a dedicated reference page covering its synopsis, options, and examples.

### Setup
- [`new`](02_myrmic-cli/01_new.md) - Scaffold a new cell crate.

### Build
- [`platforms`](02_myrmic-cli/02_platforms.md) - List available build platforms.
- [`build`](02_myrmic-cli/03_build.md) - Build a cell, workspace, or application suite.

### Runtime
- [`runtimes start`](02_myrmic-cli/04_runtimes/01_start.md) - Start a runtime instance. Aliases: `run`
- [`runtimes list`](02_myrmic-cli/04_runtimes/02_list.md) - List runtime instances.
- [`runtimes delete`](02_myrmic-cli/04_runtimes/03_delete.md) - Stop one or more runtime instances. Aliases: `stop`, `remove`, `rm`

### Deploy & Manage
- [`deploy`](02_myrmic-cli/05_deploy.md) - Deploy cells, application suites, or bridges.
- [`cells status`](02_myrmic-cli/06_cells/01_status.md) - List or inspect deployed cells. Aliases: `cell`
- [`cells teardown`](02_myrmic-cli/06_cells/02_teardown.md) - Tear down a deployed cell.
- [`cells classes list`](02_myrmic-cli/06_cells/03_classes/01_list.md) - List registered cell classes. Aliases: `class`
- [`cells classes add`](02_myrmic-cli/06_cells/03_classes/02_add.md) - Register a cell class.
- [`cells classes delete`](02_myrmic-cli/06_cells/03_classes/03_delete.md) - Remove a cell class. Aliases: `remove`, `rm`
- [`cells classes info`](02_myrmic-cli/06_cells/03_classes/04_info.md) - Show details of a cell class.
- [`network status`](02_myrmic-cli/07_network/01_status.md) - Show swarm nodes. Aliases: `nodes`, `info`
- [`tags`](02_myrmic-cli/14_tags.md) - Add and remove tags on nodes. Aliases: `tag`
- [`gateway`](02_myrmic-cli/10_gateway.md) - Start a swarm gateway node.
- [`delete`](02_myrmic-cli/11_delete.md) - Remove a deployed cell or application. Aliases: `rm`, `stop`

### Interact
- [`send`](02_myrmic-cli/08_send.md) - Send a command to a deployed cell. Aliases: `command`, `cmd`
- [`publish`](02_myrmic-cli/09_publish.md) - Publish an event into the swarm. Aliases: `event`, `pub`

### Telemetry
- [`telemetry logs`](02_myrmic-cli/12_telemetry/01_logs.md) - Print log records from the swarm.
- [`telemetry traces`](02_myrmic-cli/12_telemetry/02_traces.md) - Export swarm traces as JSON.
- [`telemetry metrics`](02_myrmic-cli/12_telemetry/03_metrics.md) - Print the latest metric values from the swarm.
- [`telemetry set-filter`](02_myrmic-cli/12_telemetry/04_set-filter.md) - Change the log filter on all connected swarm nodes.
- [`telemetry set-db-retention`](02_myrmic-cli/12_telemetry/05_set-db-retention.md) - Set the telemetry database retention period.
- [`telemetry no-db-retention`](02_myrmic-cli/12_telemetry/06_no-db-retention.md) - Disable telemetry database retention.
- [`telemetry debug`](02_myrmic-cli/12_telemetry/07_debug.md) - Stream live debug information of commands, events and logs from the swarm.

### Database
- [`database export`](02_myrmic-cli/13_database/01_export.md) - Export a database scope to a snapshot. Aliases: `db export`
- [`database import`](02_myrmic-cli/13_database/02_import.md) - Restore a snapshot into a database scope. Aliases: `db import`
