# myrmic signal-layer step

## Name
`myrmic signal-layer step` - Scaffold a Signal Layer processing step module

Aliases: `myrmic sl step`

## Synopsis
```
myrmic signal-layer step [OPTIONS] <ID>
```

## Description
Create a step module: the crate that processes the taps of drivers or other steps into new taps.
It is created at `<registry-dir>/steps/<ID>` as the crate `<ID>`, with a `descriptor.yaml`
declaring the step's configuration, inputs and outputs, a `src/lib.rs` holding the step skeleton,
and a stubbed test suite that runs with `cargo test`. The crate depends on `signal-layer-core`,
resolved through `--sdk`.

`ID` is the module id (`moving-average`), not a path; where the crate goes is `--registry-dir`. A
module with the id of a step shipped with myrmic overlays the shipped one wherever its registry is
included.

A pipeline uses the module by pointing its `build.rs` at the registry directory
(`.include("../my-modules")`) and adding the crate to its `Cargo.toml`; the command prints the
dependency line to add.

## Options
`--registry-dir <DIR>`

Base directory the module is placed under, as `steps/<ID>`. Defaults to the current directory.

`--sdk <SDK>` (alias `--repo`)

Where the module resolves `signal-layer-core` from. Accepts a path to a local checkout (a path
dependency, no network) or a version/revision (a registry or git dependency). Defaults to the
revision the CLI was built from.

`-v` / `--verbose`

Verbose mode; repeat (`-vv`) for more detail.

`-h`, `--help`

Print help information.

## Examples

Scaffold a step into a module registry, against a local checkout, and run its stubbed tests:

```bash
myrmic signal-layer step moving-average --registry-dir ~/my-modules --sdk ~/myrmic
cd ~/my-modules/steps/moving-average && cargo test
```

## See also
- [Write your own step](../../../05_guides/11_signal-layer/08_write-your-own-step.md) - what to
  fill in after scaffolding.
- [Steps](../../04_signal-layer/04_steps.md) - the descriptor format and the step contract.
- [`signal-layer new`](01_new.md) - the pipeline project that uses the module.
