# myrmic cells status

## Name
`myrmic cells status` - List and inspect deployed cells

## Synopsis
```
myrmic cells status [SRI/SRN]...
```

## Description
List all registered cells, or inspect specific ones by passing one or more SRIs (UUID) or SRN names. Each match is rendered with its whole spawn subtree.

If no `SRI/SRN` is provided, all deployed cells are listed.

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. List all registered cells:

```bash
myrmic cells
```

2. Inspect a specific cell:

```bash
myrmic cells status my-cell
```
