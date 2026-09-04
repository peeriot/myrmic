# myrmic runtimes list

## Name
`myrmic runtimes list` - List Myrmic runtime instances

## Synopsis
```
myrmic runtimes list [OPTIONS]
```

## Description
List runtime instances started by the Myrmic CLI. Shows the name, status (`running`, `stale`, or `invalid`), and PID of each.

## Options
`--pid-path PATH`

For advanced usage, specifies the location of runtime PID files. Pass a directory to list all of them, or a specific `.pid` file to inspect a single one. Use this if you started your runtime with a custom `--pid-path`.

Defaults to `$XDG_RUNTIME_DIR/myrmic/` when set, otherwise `<tempdir>/myrmic-<user>/`.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. List all runtime instances:

```bash
myrmic runtimes list
```
