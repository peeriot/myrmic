# myrmic tags

## Name
`myrmic tags` - Add and remove tags on nodes

Aliases: `tag`

## Synopsis
```
myrmic tags [OPTIONS] [NODE]...
```

## Description
Adds and removes tags on running nodes, without restarting them.

A node has one set of tags, and it decides two things: which cells may be placed on it (a cell requiring `gpu` only runs on a node carrying `gpu`), and which data it replicates, since a replication set names the tags of the nodes that should hold a replica. A node's configuration gives it the tags it starts with — `tags` in the [runtime configuration](../01_configuration/01_runtime-configuration.md), or `--tag` on [`runtimes start`](04_runtimes/01_start.md). This command carries tags on top of those and drops tags it should not have.

Changes are stored per node rather than per plugin, and reach a running node on their own: a Linux runtime applies one within seconds, an embedded node on its next registration round. Until a node has picked a change up, this command shows it as `(pending)` or `(removing)` rather than as fact.

With no arguments, lists every node on the network and its tags. Given nodes but no `--tag`/`--exclude`, shows just those nodes.

`NODE` names a node by the name it registered under, by its runtime id, or by any unique prefix of that id. A leading `@` is accepted on all three, matching how a replication set pins to one node. Repeat it to retag several nodes at once; all of the changes are written in a single transaction.

Tags naming what a node *is* — its platform (`linux`, `esp32c6`), its hardware (`ble`, `gpio`, `psram`), its own `@<runtime id>` — are facts rather than preferences. They can never be removed, and a node keeps them however it is tagged. Excluding one is recorded but has no effect, and the tag keeps its `(removing)` marker for as long as the exclusion stands: only the node knows which of its tags are facts, so this command cannot tell a removal a node has yet to apply from one it will always refuse. `--reset` clears such an exclusion.

## Options
`--tag TAG` / `-t TAG`

A tag the nodes should carry. Can be specified multiple times.

`--exclude TAG` / `-e TAG`

A tag the nodes should not carry, whatever its origin — including one from the node's configuration file. Can be specified multiple times.

Excluding a tag records that the node must not carry it, rather than merely undoing an earlier `--tag`. Use `--reset` to discard the changes entirely.

`--reset`

Forget every tag change made to the named nodes, leaving them with the tags their configuration gives them. Cannot be combined with `--tag` or `--exclude`.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. List every node and its tags:

```bash
myrmic tags
```

2. Tag two nodes as belonging to a region:

```bash
myrmic tags -t hello -t region-1 @node-1 @node-2
```

3. Stop a node from being treated as part of a region:

```bash
myrmic tags -e region-2 @node-3
```

4. Show one node, addressed by a prefix of its runtime id:

```bash
myrmic tags @6483ae05
```

5. Return a node to the tags its configuration gives it:

```bash
myrmic tags --reset @node-1
```

## See Also
- [`network status`](07_network/01_status.md) - shows the same nodes alongside their kind and topology.
- [`runtimes start`](04_runtimes/01_start.md) - sets the tags a runtime starts with.
