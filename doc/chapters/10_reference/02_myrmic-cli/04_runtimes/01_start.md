# myrmic runtimes start

## Name
`myrmic runtimes start` - Start a Myrmic runtime

Aliases: `run`

## Synopsis
```
myrmic runtimes start [OPTIONS] [PATH]
```

## Description
Start a Myrmic runtime. Runs in the foreground by default.

Once the runtime can run cells, it logs a readiness line:

```text
INFO  runtime "<NAME>" ready (<id>)
```

`PATH` is an optional YAML configuration file for the runtime. If not provided, the runtime starts with the built-in default configuration.

See [Runtime Configuration](../../01_configuration/01_runtime-configuration.md) for all options available in the configuration file.

## Options
`--name NAME`

Sets the runtime instance name. Defaults to `default`.

`--tag TAG` / `-t TAG`

Adds a capability tag to this runtime instance. Can be specified multiple times to add multiple tags. Merged with any tags in the configuration file.

`--connect ENDPOINT`

A peer endpoint this runtime connects to, e.g. `tcp/10.0.0.5:7447`. Can be specified multiple times. Merged with `zenoh.connect.endpoints` from the configuration file; multicast discovery stays as the file configures it. A global flag, so it goes before `runtimes start`; unlike for the other commands, `DEFAULT_MYRMIC_CONNECT` does not apply to a runtime.

`--listen ENDPOINT`

An endpoint this runtime listens on, e.g. `tcp/0.0.0.0:7447`. Can be specified multiple times. Merged with `zenoh.listen.endpoints` from the configuration file.

`--multicast-group ADDRESS:PORT`

The multicast group this runtime scouts on, overriding `zenoh.scouting.multicast.address` from the configuration file. A global flag, so it goes before `runtimes start`; unlike for the other commands, `DEFAULT_MYRMIC_MULTICAST_GROUP` does not apply to a runtime.

`--detached` / `-d`

Start the runtime as a background daemon. The command returns once the runtime is ready, so a deploy right after it can place cells on it. If the runtime exits before it is ready, the command fails and names the directory with its logs.

`--pid-path PATH`

For advanced usage, specifies the location of the runtime PID file. Pass a directory to place it inside as `<NAME>.pid`, or a specific path to use as the PID file directly.

Defaults to `$XDG_RUNTIME_DIR/myrmic/` when set, otherwise `<tempdir>/myrmic-<user>/`.

`--tmp`

Use an ephemeral in-memory database that is discarded when the runtime stops. Overrides any database directory set in the configuration file. Without this, the runtime uses a persistent database under the data folder, keyed by its stable node id.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Start a runtime with a custom name in the background:

```bash
myrmic runtimes start --name my-runtime --detached
```

2. Start a runtime with capability tags:

```bash
myrmic runtimes start --tag my-tag-1 --tag my-tag-2
```

3. Start a runtime from a configuration file:

```bash
myrmic runtimes start ./my-runtime.yml
```

4. Start a throwaway runtime with an in-memory database:

```bash
myrmic runtimes start --tmp
```

5. Start a runtime in a container that reaches its swarm through one peer:

```bash
myrmic --connect tcp/10.0.0.5:7447 runtimes start --tag gate-nfc
```
