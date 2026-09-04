# myrmic platforms

## Name
`myrmic platforms` - List available build platforms

## Synopsis
```
myrmic platforms [OPTIONS]
```

## Description
List all build platforms available in this version of the CLI. Each line shows the platform name and any accepted aliases.

Use any platform listed here with `--platform` in [`myrmic build`](03_build.md) and [`myrmic deploy`](05_deploy.md).

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Check which platforms are available before building:

```bash
myrmic platforms
```
