# myrmic runtimes delete

## Name
`myrmic runtimes delete` - Stop one or more runtime instances

Aliases: `stop`, `remove`, `rm`

## Synopsis
```
myrmic runtimes delete [OPTIONS] [NAME]...
```

## Description
Stop one or more runtime instances by NAME.

If a runtime process is gone but its PID file remains, the stale file is removed and a warning is printed.

## Options
`--pid-path PATH`

For advanced usage, specifies the location of runtime PID files. Use this if you started your runtime with a custom `--pid-path`.

Defaults to `$XDG_RUNTIME_DIR/myrmic/` when set, otherwise `<tempdir>/myrmic-<user>/`.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Stop multiple runtimes:

```bash
myrmic runtimes delete runtime-1 runtime-2
```
