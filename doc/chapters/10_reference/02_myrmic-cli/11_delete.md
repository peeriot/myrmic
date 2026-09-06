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

In a terminal, `delete` asks what to remove when the target could mean more than one thing (the
cell, the application it belongs to, or its descendants). Run without a terminal it does not
prompt; pass the choice as one of these flags, or the command stops with `refusing to prompt
without a terminal`.

`--cell`

Delete just this cell.

`--app`

Delete the whole application the target belongs to, and all its cells.

`--branch`

Delete the cell together with its descendants.

`--children`

Delete the target's descendants but keep the target itself.

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
