# myrmic cells classes delete

## Name
`myrmic cells classes delete` - Remove a cell class

Aliases: `remove`, `rm`

## Synopsis
```
myrmic cells classes delete [NAME]
```

## Description
Removes the name `NAME`, then the class it pointed to, unless something still references the class: another name, a cell instance, the restart spec of a root cell, or a wasm class that spawns it or was compiled to it. A class that stays is reported with what references it.

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
