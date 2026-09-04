# myrmic cells classes info

## Name
`myrmic cells classes info` - Show details for a cell class

## Synopsis
```
myrmic cells classes info [NAME]
```

## Description
Show details for the cell class identified by `NAME`, including the hash of its registered wasm binary and any AOT artifacts with their target platform, compiled binary hash, and metadata file hash.

## Options
`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Show class details:

```bash
myrmic cells classes info my-cell-class
```
