# myrmic cells teardown

## Name
`myrmic cells teardown` - Undeploy a cell and erase its instance

## Synopsis
```
myrmic cells teardown [OPTIONS] [SRI/SRN]
```

## Description
Purges a running cell identified by its SRI (UUID) or SRN name. Unlike [`myrmic delete`](../11_delete.md), which only stops the cell, teardown also removes the instance record from the data layer, so redeploying after a teardown starts the cell as if it were brand new.

## Options
`--remove-class`

Also remove the cell's class after teardown. For a root cell, the class name equal to its SRN (the name `myrmic deploy` gave the class) is removed first; then the class itself goes, unless something still references it, such as another name or another cell.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples

1. teardown a cell and remove its class if nothing else uses it:

```bash
myrmic cells teardown my-cell --remove-class
```
