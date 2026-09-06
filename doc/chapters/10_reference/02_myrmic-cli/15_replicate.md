# myrmic replicate

## Name
`myrmic replicate` - Configure which nodes replicate which data

Aliases: `replicas`, `replica`, `rep`

## Synopsis
```
myrmic replicate [OPTIONS] [IDENTIFIER]
myrmic replicate --file PATH [--prune]
```

## Description

> **Work in progress**
> Replication sets replicate a Cell's *data*, not the Cell. The Cell keeps running in one place; after Node loss it does not resume by itself, and this command is not yet covered by the [guarantee contract](../../08_guarantees.md). What it gives you today is the manual failover the [roadmap](../../09_roadmap.md) describes: the data survives the Node, and the Cell can be redeployed where a replica lives.

Configures replication sets. An entry pairs something to replicate - an application, a cell, or a scope - with the tags of the nodes that should hold a replica. A node holds a replica when it carries any one of an entry's tags, so the configuration never names a node; it names what a node must be. Which tags a node carries comes from `tags` in its [runtime configuration](../01_configuration/01_runtime-configuration.md) and from [`myrmic tags`](14_tags.md), so retagging a node changes what it replicates without touching the replication sets.

With no arguments, lists every configured replication set. Given an identifier but no `--tag`/`--exclude`, shows just that entry.

### Identifiers

`IDENTIFIER` names what to replicate. Its shape decides how it is read:

- `app:<name>` - every scope of every cell registered to the application `<name>`. Membership is resolved by each node when it applies the configuration, not when the entry is written, so a cell deployed into the application later is picked up without reconfiguration.
- A UUID - a cell's SRI, taken verbatim.
- Anything else, such as `chatty` or `chatty/server` - a cell's SRN path, folded to its SRI. A path with an empty segment, such as `chatty/`, is rejected.
- `scope:<namespace>`, `scope:<namespace>/<database>`, or `scope:<namespace>/<database>/<schema>` - a slice of the [scope hierarchy](../03_myrmic-sdk/05_state-and-storage/01_storage-scopes.md), named directly.

A cell entry, whether written as SRI or SRN, covers the cell's own data: its private scope under every schema. Data in a public scope is shared between cells and belongs to none of them, so it is not covered by any cell entry - name it with `scope:` instead.

An entry written as an SRN is stored under the cell's SRI, but keeps the name you typed for display.

### Output

The listing has one row per target, with the tags configured for it:

```
TARGET                          TAGS
app:chatty                      region-1, region-2
chatty/server                   @6483ae05b2c94f10
scope:application-data/metrics  @a0b1c2d3e4f50617 (provisional)
```

A tag marked `(provisional)` is not configured. It names a node that promoted itself into a replica for that scope because no configured replica could be located. It shares the target's row so that a divergence from intent is visible at a glance. With nothing configured and no provisional custody, the command prints `no replication sets configured`.

## Options
`--tag TAG` / `-t TAG`

A tag of the nodes that should hold a replica. Can be specified multiple times. Added to the tags the entry already has. Requires an identifier.

System tags are accepted here: `@<runtime id>` pins a replica to that one runtime. Match is exact, so write the full runtime id as [`myrmic tags`](14_tags.md) shows it.

`--exclude TAG` / `-e TAG`

A tag to stop replicating on. Can be specified multiple times. Removing an entry's last tag drops the entry. Requires an identifier.

`--file PATH` / `-f PATH`

Apply a file of `identifier: [tag, ...]` entries, in YAML or JSON. Each identifier's tags replace whatever was configured for it; an empty list drops the entry. Identifiers the file does not mention are left alone. Cannot be combined with an identifier, `--tag`, or `--exclude`.

`--prune`

With `--file`, also drop every entry the file does not mention.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

All changes of one invocation are written in a single transaction, so a partial apply cannot leave the network disagreeing about what it should be replicating.

## Examples
1. List every replication set:

```bash
myrmic replicate
```

2. Replicate a whole application on the nodes of a region:

```bash
myrmic replicate app:chatty -t region-1
```

3. Replicate one cell, named by its SRN, on two regions:

```bash
myrmic replicate chatty/server -t region-1 -t region-2
```

4. Pin a cell's replica to one runtime:

```bash
myrmic replicate chatty/server -t @6483ae05b2c94f10
```

5. Replicate a public scope shared by several cells:

```bash
myrmic replicate scope:application-data/metrics -t region-1
```

6. Stop replicating an application on a region:

```bash
myrmic replicate app:chatty -e region-2
```

7. Show one entry:

```bash
myrmic replicate app:chatty
```

8. Apply a file and drop every entry it does not mention:

```yaml
# replicas.yml
app:chatty: [region-1, region-2]
chatty/server: ['@6483ae05b2c94f10']
scope:application-data/metrics: [region-1]
```

```bash
myrmic replicate -f replicas.yml --prune
```

