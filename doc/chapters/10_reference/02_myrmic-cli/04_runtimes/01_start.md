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

`PATH` is an optional YAML configuration file for the runtime. If not provided, the runtime starts with the built-in default configuration.

See [Runtime Configuration](../../01_configuration/01_runtime-configuration.md) for all options available in the configuration file.

## Options
`--name NAME`

Sets the runtime instance name. Defaults to `default`.

`--tag TAG` / `-t TAG`

Adds a capability tag to this runtime instance. Can be specified multiple times to add multiple tags. Merged with any tags in the configuration file.

`--detached` / `-d`

Start the runtime as a background daemon.

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
