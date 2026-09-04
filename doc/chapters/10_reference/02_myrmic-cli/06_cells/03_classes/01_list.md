# myrmic cells classes list

## Name
`myrmic cells classes list` - List registered cell classes

## Synopsis
```
myrmic cells classes list
```

## Description
List all cell classes registered in the swarm. Each entry shows the class name and the hash of its registered wasm binary, and any AOT artifacts with the target platform, the hash of the compiled native binary, and the hash of the metadata file.

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
