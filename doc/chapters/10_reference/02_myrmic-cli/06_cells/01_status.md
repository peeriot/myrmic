# myrmic cells status

## Name
`myrmic cells status` - List and inspect deployed cells

## Synopsis
```
myrmic cells status [OPTIONS] [SRI/SRN]...
myrmic cells [OPTIONS] [SRI/SRN]...
```

## Description
List all registered cells, or inspect specific ones by passing one or more SRIs (UUID) or SRN names. Each match is rendered with its whole spawn subtree.

If no `SRI/SRN` is provided, all deployed cells are listed.

`status` is the default subcommand, so `myrmic cells` accepts the same arguments directly.

In a terminal the listing is live: it refreshes every `--interval` until interrupted with Ctrl+C, and the last state stays on screen afterwards. A cell that appears - or comes back with a new generation after a respawn - fades in green; a cell that disappears fades out struck through in red before its row goes. If a refresh fails, the previous listing stays up and the error is shown above it until the next refresh succeeds.

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
1. Watch all registered cells:

```bash
myrmic cells
```

2. Inspect a specific cell:

```bash
myrmic cells status my-cell
```

3. Print the listing once, for a script or a quick look:

```bash
myrmic cells --once
```
