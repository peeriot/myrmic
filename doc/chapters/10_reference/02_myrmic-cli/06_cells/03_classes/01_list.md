# myrmic cells classes list

## Name
`myrmic cells classes list` - List registered cell classes

## Synopsis
```
myrmic cells classes list
```

## Description
List all cell classes registered in the swarm. Each entry shows the names bound to the class (as synonyms, `a = b`, or the start of its hash if it has none), its hash, and its kind: a wasm class with its AOT compilations per platform and the classes it spawns, or an AOT binary with its platform.

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. List all registered classes:

```bash
myrmic cells classes
```
