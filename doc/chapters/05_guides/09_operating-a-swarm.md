# Operating a swarm

Once your cells are built and deployed, you are operating a swarm: one or more runtimes on a network, joining into a single system, running your cells and holding their data. This guide covers what that looks like in practice - how to tell a runtime is ready, how nodes find each other and where cells land, how the swarm behaves when a node pauses or a clock drifts, what it costs to run, and the rough edges to plan around.

This guide is about operating Linux nodes. The figures here are lab measurements on small cloud instances (2 to 4 vCPU, Debian 12 and Ubuntu 24.04); treat them as what to expect on comparable hardware, not as guarantees, and measure your own workload.

## Knowing a runtime is ready

`myrmic runtimes start` does not return: it runs the runtime in the foreground until you stop it. The runtime is ready to take deployments once it logs its readiness line, about two seconds after you start it:

```text
INFO  runtime "default" ready (<id>)
```

On the first start you may also see one or two `WARN` lines about missing prior state - these are normal and can be ignored.

From another terminal, `myrmic runtimes list` confirms it independently: a runtime it reports as `running` is one you can deploy to. See the [`myrmic runtimes start`](../10_reference/02_myrmic-cli/04_runtimes/01_start.md) and [`myrmic runtimes list`](../10_reference/02_myrmic-cli/04_runtimes/02_list.md) references for synopsis, options and examples.

## Discovery, identity, and placement

Runtimes find each other on their own. Every runtime that can reach the others on the network joins one swarm, with no explicit join step, so two machines on the same segment form a swarm whether or not you intended it. `myrmic network status` shows the nodes a runtime currently sees. See the [`myrmic network status` reference](../10_reference/02_myrmic-cli/07_network/01_status.md) for synopsis, options and examples.

A cell's SRN is swarm-wide. The same SRN names the same cell everywhere, so deploying a name that already exists addresses the existing cell rather than creating a second copy. Give distinct cells distinct names.

When you deploy, the swarm places the cell on a node that can run it, which is not necessarily the machine you ran the CLI on. `myrmic cells status` shows each deployed cell and the runtime it runs on, so you can see where a deploy landed. See the [`myrmic cells status` reference](../10_reference/02_myrmic-cli/06_cells/01_status.md) for synopsis, options and examples.

To constrain where a cell runs, tag the nodes that may host it and require those tags at deploy time with `--tag`. A cell that requires a tag is placed only on a node carrying it. See the [`myrmic deploy` reference](../10_reference/02_myrmic-cli/05_deploy.md) and the [`myrmic tags` reference](../10_reference/02_myrmic-cli/14_tags.md) for details.

To keep separate groups of machines from merging into one swarm, put them on separate network segments. Any runtime on the same segment joins, and this preview has no authentication between nodes, so the segment is the trust boundary. See the [Security](../11_security.md) page for the trust assumptions.

## Timing and liveness

A node that goes silent is declared lost after roughly 60 to 70 seconds. A shorter pause, under about 45 seconds, is not treated as a loss, so a node that is briefly busy or paused stays in the swarm.

Two views update at different speeds. `network status` reflects transport liveness within seconds, while the cell view from `cells status` changes only after the swarm runs its periodic hygiene, so it lags behind. A node can already be gone from `network status` while it still appears under `cells status` for a short while.

Messages to an unreachable node are not dropped. Commands wait in the node's mailbox and are delivered once it reconnects, and a partition that heals delivers each command exactly once.

If the node that holds one of the swarm's internal tables (the shared records that track what is deployed and where) disappears, operations that need it - `cells`, `send`, `network status` - can fail for up to a minute while the swarm moves that table to another node. This clears by itself; retrying after a few seconds succeeds.

## Durability and recovery

Cell state is durable across a process restart. In 0.4.0, state durability was verified against `kill -9`, power loss, and a full disk: acknowledged writes survived and the database recovered on the next start.

By default a cell's state is held as a single copy, so it is durable across a restart of its node but not across the loss of that node. Configure [replication](../10_reference/02_myrmic-cli/15_replicate.md) for a scope if you need state to outlive a node. See the [Guarantees](../08_guarantees.md) page for the exact durability contract.

After a hard stop, `myrmic runtimes list` may show a runtime as `stale`: its PID file outlived the process. Starting a runtime of the same name reclaims the entry. Note also that after a hard reset, runtime log lines can lag behind the state that was actually persisted, so trust the recovered database over the tail of the log.

## Clock synchronization

Runtimes stamp messages with a hybrid logical clock and reject messages from a peer whose clock is more than 500 ms ahead of their own. Run NTP or PTP on every node so clocks stay within that bound.

A forward clock jump has a lasting effect: once a node's clock runs ahead, its messages keep being rejected even after the clock is corrected, until the runtime is restarted. After correcting a clock that had jumped forward, restart the runtime on that node.

## Disk and storage

A runtime keeps its database, identity and logs under its data directory. See [what Myrmic writes to your machine](../01_quickstart/01_installation.md#what-myrmic-writes-to-your-machine) for the exact locations.

Watch free space. If the disk fills, the database enters a state it does not recover from on its own: the runtime keeps reporting success while writes no longer persist, and it must be restarted after space is freed. Monitor free space on nodes that hold data rather than relying on write calls to report the problem.

Retained telemetry is the main driver of database growth over time, and retention is off by default. See [Telemetry retention](./10_observability.md#telemetry-retention) for how to set a retention period and what it costs.

## Running in containers

A runtime in a container is invisible to the LAN unless it shares the host's network, because swarm discovery relies on reaching peers on the local segment. Run the container with host networking:

```bash
docker run --network host <image>
```

Without it, the runtime starts and looks healthy but never joins the swarm on the host's network.

## Running as a service

To keep a runtime up across reboots, run it under systemd. Because `myrmic runtimes start` stays in the foreground, a plain service type works:

```ini
[Unit]
Description=Myrmic runtime
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/bin/myrmic runtimes start
Restart=on-failure
User=myrmic

[Install]
WantedBy=multi-user.target
```

Two things differ from an interactive start:

- **PID file and data locations.** A service usually runs without `$XDG_RUNTIME_DIR`, so its PID file goes to `$TMPDIR/myrmic-<user>/` instead, and its data directory is that of the service user. See [what Myrmic writes to your machine](../01_quickstart/01_installation.md#what-myrmic-writes-to-your-machine).
- **CLI visibility.** `myrmic runtimes list` and `myrmic runtimes stop` only see runtimes started by the same user. A runtime running as another service user is managed with `systemctl`, not with the CLI.

For cells to come back when the service restarts, deploy them with a restart policy (`--policy on-error` or `--policy always`) rather than the default `never`. See the [`myrmic deploy` reference](../10_reference/02_myrmic-cli/05_deploy.md) for the policy options.

## Resource footprint

A runtime doing nothing but holding one deployed cell is small. Measured on a 4 vCPU instance:

| Metric | Idle runtime, one cell |
|---|---|
| Resident memory (RSS) | ~49 MB |
| CPU | ~0.7 % |
| Data directory on disk | ~164 KB |

Memory grows once the runtime is doing real work, and telemetry retention is the main driver: retained logs and traces are kept for querying rather than dropped. In one three-hour soak with retention enabled, resident memory reached about 84 MB. That is a single sample taken at the end of the run, so read it as an indication that retention pushes memory into the tens of megabytes over hours, not as a precise growth curve. If a runtime's memory matters to you, keep telemetry retention modest and measure it under your own workload.

## Current limitations

Two operational states in this preview need a manual restart to clear:

- A full disk requires a runtime restart after space is freed (see [Disk and storage](#disk-and-storage)).
- A forward clock jump requires a runtime restart after the clock is corrected (see [Clock synchronization](#clock-synchronization)).

## See also

- [Observability](./10_observability.md)
- [Guarantees](../08_guarantees.md)
- [Security](../11_security.md)
- [Myrmic CLI reference](../10_reference/02_myrmic-cli.md)
- [Runtime configuration reference](../10_reference/01_configuration/01_runtime-configuration.md)
