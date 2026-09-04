# myrmic delete

## Name
`myrmic delete` - Stops a deployed cell or application

Aliases: `rm`, `stop`

## Synopsis
```
myrmic delete [OPTIONS] [TARGET]
```

## Description
Stops a deployed cell or application.

- If `TARGET` is an application name - stops the entire application and all its cells.
- If `TARGET` is a cell SRI or SRN - stops the cell instance.

Deleting a cell that belongs to an application is not allowed.

To purge a cell and its instance data, use [`myrmic cells teardown`](06_cells/02_teardown.md) instead.

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Delete a cell:

```bash
myrmic delete my-cell
```

2. Delete an application and all its cells:

```bash
myrmic delete my-app
```
