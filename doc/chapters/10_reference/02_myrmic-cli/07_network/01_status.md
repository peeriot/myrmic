# myrmic network status

## Name
`myrmic network status` - Show swarm nodes

Aliases: `info`

## Synopsis
```
myrmic network status
```

## Description
Lists all connected hosts in the swarm, their identifiers, roles and the swarm topology.

Since the swarm uses [Zenoh](https://zenoh.io) for messaging, runtimes appear as peers or routers depending on how they were configured at startup - see [`zenoh` in the runtime configuration](../../01_configuration/01_runtime-configuration.md#zenoh-advanced).

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Show swarm nodes:

```bash
myrmic network status
```
