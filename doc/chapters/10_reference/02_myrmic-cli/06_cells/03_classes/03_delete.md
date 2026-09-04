# myrmic cells classes delete

## Name
`myrmic cells classes delete` - Remove a cell class

Aliases: `remove`, `rm`

## Synopsis
```
myrmic cells classes delete [NAME]
```

## Description
Removes the cell class identified by `NAME` from the data layer.

Fails if the class has any instances. Delete all instances first.

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Remove a class:

```bash
myrmic cells classes delete my-cell-class
```
