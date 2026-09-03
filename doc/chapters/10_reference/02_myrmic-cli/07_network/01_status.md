# myrmic network status

## Name
`myrmic network status` - Show swarm nodes

Aliases: `info`

## Synopsis
```
myrmic network status [OPTIONS]
myrmic network [OPTIONS]
```

## Description
Lists all connected hosts in the swarm, their identifiers, roles and the swarm topology.

Since the swarm uses [Zenoh](https://zenoh.io) for messaging, runtimes appear as peers or routers depending on how they were configured at startup - see [`zenoh` in the runtime configuration](../../01_configuration/01_runtime-configuration.md#zenoh-advanced).

`status` is the default subcommand, so `myrmic network` accepts the same arguments directly.

In a terminal the listing is live: it refreshes every `--interval` until interrupted with Ctrl+C, and the last state stays on screen afterwards. A node that joins fades in green; a node that leaves fades out struck through in red before its row goes. If a refresh fails, the previous listing stays up and the error is shown above it until the next refresh succeeds.

When the output is not a terminal (piped into another command, say) the listing is printed once, as if `--once` had been given.

## Options
`--once`

Print the listing once and exit instead of refreshing until interrupted.

`--interval DURATION`

Time between refreshes while live, e.g. `500ms`, `2.5s`, `1m`. Defaults to `2.5s`.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Watch the swarm nodes:

```bash
myrmic network
```

2. Print the nodes once:

```bash
myrmic network status --once
```
