# myrmic cells status

## Name
`myrmic cells status` - List and inspect deployed cells

## Synopsis
```
myrmic cells status [OPTIONS] [SRI/SRN]...
myrmic cells [OPTIONS] [SRI/SRN]...
```

## Description
List all registered cells, or inspect specific ones by passing one or more SRIs (UUID) or SRN names. Each match is rendered with its whole spawn subtree.

If no `SRI/SRN` is provided, all deployed cells are listed.

`status` is the default subcommand, so `myrmic cells` accepts the same arguments directly.

In a terminal the listing is live: it refreshes every `--interval` until interrupted with Ctrl+C, and the last state stays on screen afterwards. A cell that appears - or comes back with a new generation after a respawn - fades in green; a cell that disappears fades out struck through in red before its row goes. If a refresh fails, the previous listing stays up and the error is shown above it until the next refresh succeeds.

When the output is not a terminal (piped into another command, say) the listing is printed once, as if `--once` had been given.

## Output
One row per registered cell, indented into its spawn tree; without arguments the rows are grouped by application:

- `cell` - the cell's own name segment.
- `sri` - the cell's unique id.
- `kind` - `wasm`, `bridge`, or `N/A` for a cell that is claimed but not yet deployed.
- `runtime` - the execution runtime the cell is placed on, shown as the first 8 characters of its id, or more when 8 are not enough to tell it apart from another runtime in the listing. The characters that make it unique are highlighted in a terminal and bracketed outside one, as in `[8]30d3436` - the brackets mark the unique prefix, here the single leading character, not a count.
- `age` - how long the current incarnation has been placed; a respawn resets it.
- `policy` - the restart policy in force, as [`myrmic deploy --policy`](../05_deploy.md) spells it: `never`, `on-error` or `always`. Only root cells carry one, and a wasm root deployed without a policy shows `never`. A root that is being restarted shows the policy it comes back under even while its `kind` reads `N/A`: the stored spec outlives the placement row, and the restart replays it. The column lags at both ends of a cell's life - a first deploy reads `—` while the cell is still being placed and can read `never` for one refresh before an `on-error` or `always` policy is stored, and a teardown erases the spec just before it removes the row, so an `always` root can read `never` on its way out. `—` means there is no restart policy to read for that row: a spawned cell and a bridge are never restarted by policy, and a row that has not finished registering has nothing to read.
- `class` - the cell class the instance was created from.
- `srn` - the cell's full human-readable name. A chain that cannot be walked back to a named root keeps its known tail behind `…/`.

`—` in any column means the row has no value to show there.

## Options
`--once`

Print the listing once and exit instead of refreshing until interrupted.

`--interval DURATION`

Time between refreshes while live, e.g. `500ms`, `2.5s`, `1m`. Defaults to `2.5s`.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Watch all registered cells:

```bash
myrmic cells
```

2. Inspect a specific cell:

```bash
myrmic cells status my-cell
```

3. Print the listing once, for a script or a quick look:

```bash
myrmic cells --once
```
