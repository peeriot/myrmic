# myrmic cells classes delete

## Name
`myrmic cells classes delete` - Remove a cell class

Aliases: `remove`, `rm`

## Synopsis
```
myrmic cells classes delete [OPTIONS] [CLASS_REF]
```

## Description
Removes the name `CLASS_REF`, then the class it pointed to, unless something still references the class: another name, a cell instance, the restart spec of a root cell, or a wasm class that spawns it or was compiled to it. A class that stays is reported with what references it.

With `--ref-is-hash`, `CLASS_REF` is the class's hash or a unique prefix of it, as [`myrmic cells classes list`](./01_list.md) prints it. This reaches classes that no longer have a name, for example the previous build of a redeployed cell. No name is removed, so a class that still has one is kept and reported.

## Options
`-r` / `--ref-is-hash`

Treat `CLASS_REF` as a class hash, or a unique prefix of one, instead of a name.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Remove a class:

```bash
myrmic cells classes delete my-cell-class
```

2. Remove a class without a name by its hash prefix:

```bash
myrmic cells classes delete --ref-is-hash 3f9a2c
```
