# Part 1 - The Sensor and Its Node

This is Part 1 of the [Resilience](../03_resilience.md) tutorial. You start three runtimes that stand in for three machines, pin the sensor to the one with the probe, and then kill that runtime. Along the way you learn about *placement tags* - how a cell says where it may run - and *restart policies* - what the swarm does when a cell's runtime dies.

---

## Step 1 - Start Three Runtimes

A runtime carries *tags*: words that describe what the machine is or has. Some it gets for free - every Linux runtime carries `linux`, and every runtime carries its own id as `@<id>`. Others you give it when you start it. A cell, in turn, can *require* tags, and it is only ever placed on a runtime that carries all of them.

In Terminal 1, start the three runtimes. Each gets a name and one tag. `--detached` puts them in the background so one terminal is enough:

```bash
myrmic runtimes start -n node1 --tag soil-probe --detached
myrmic runtimes start -n node2 --tag compute --detached
myrmic runtimes start -n node3 --tag compute --detached
```

Expected output, once per runtime:

```text
INFO  runtime "node1" pid file: /run/user/1000/myrmic/node1.pid
INFO  Starting runtime "node1"(112a7f3f15aa4f7ead00258a4c8c609a)
```

Give them a few seconds to find each other, then look at the swarm:

```bash
myrmic network status
```

```text
Discovered 3 node(s)

  #  name   kind   id          tags
  0  node1  linux  [1]12a7f3f  soil-probe, @112a7f3f15aa4f7ead00258a4c8c609a, linux
  1  node2  linux  [c]a157d28  compute, @ca157d28d8cf44689a97832a30327a8c, linux
  2  node3  linux  [4]f741c9b  compute, @4f741c9b645144daa3d0e8776399c7bf, linux
```

Three nodes, one swarm. `node1` is the only one that carries `soil-probe`; `node2` and `node3` both carry `compute`. Keep this table in mind: the `id` column is how the swarm names a node, and you will translate ids back to names more than once.

---

## Step 2 - Pin the Sensor to Its Node

Go to your greenhouse workspace and deploy the sensor. Two new flags appear:

```bash
cd greenhouse
myrmic deploy moisture-sensor -t soil-probe --policy always
```

```text
INFO  deploying cell (srn = moisture-sensor, sri = 365d6cfe-e9c2-5914-bfed-3a17d4ecd6da)
INFO  deployed cell (srn = moisture-sensor, sri = 365d6cfe-e9c2-5914-bfed-3a17d4ecd6da)
```

- `-t soil-probe` is the cell's *requirement*. Only `node1` carries that tag, so only `node1` qualifies - the probe is wired to that machine and nowhere else.
- `--policy always` is the cell's *restart policy*: if the cell stops for any reason other than you removing it, the swarm starts it again. The default is `never`.

```bash
myrmic cells
```

```text
  cell             sri                                   kind  runtime     age  policy  class            srn
──── moisture-sensor ───────────────────────────────────────────────────────────────────────────────────────────────────
  moisture-sensor  365d6cfe-e9c2-5914-bfed-3a17d4ecd6da  wasm  [1]12a7f3f  14s  always  moisture-sensor  moisture-sensor
```

The `runtime` column says `[1]12a7f3f` - `node1`, as it must. Open Terminal 2 and listen to it:

```bash
myrmic subscribe moisture
```

```text
[2026-09-06T18:12:33.728Z] event=moisture sender=365d6cfe-... payload=8 bytes
59.79998

[2026-09-06T18:12:34.730Z] event=moisture sender=365d6cfe-... payload=8 bytes
59.39998
```

One reading a second, the mock weather slowly drying the soil.

---

## Step 3 - Kill the Node, Bring It Back

Now pull the plug on the Raspberry Pi. Find `node1`'s process id and kill it the way a power cut would, with no chance to shut down cleanly:

```bash
myrmic runtimes list
```

```text
INFO  3 local runtime(s)
node1	running	pid=99396
node2	running	pid=99413
node3	running	pid=99430
```

```bash
kill -9 99396
```

Terminal 2 falls silent at once. The sensor ran on `node1` and `node1` is gone; there is nothing left to publish.

Wait as long as you like - nothing changes. The sensor has a restart policy, but restarting it means placing it on a runtime that carries `soil-probe`, and there is no such runtime in the swarm right now. `myrmic cells` will stop listing the sensor after about a minute, once the swarm has given `node1` up; `myrmic network status` shows two nodes. A restart policy cannot conjure up hardware.

Now the electrician arrives. Start `node1` again, with the same name and tag:

```bash
myrmic runtimes start -n node1 --tag soil-probe --detached
```

Watch Terminal 2. **About 20 seconds** after the runtime is up, the readings resume:

```text
[2026-09-06T18:13:19.431Z] event=moisture sender=365d6cfe-... payload=9 bytes
63.799995

[2026-09-06T18:13:20.433Z] event=moisture sender=365d6cfe-... payload=9 bytes
63.399994
```

Nobody redeployed anything. The swarm remembered that the sensor should be running, saw a runtime with `soil-probe` reappear, and put the sensor back on it - that is `--policy always` doing its job. `myrmic cells` lists it again, with a fresh `age`.

You may notice the reading jumped - from the high 50s before the kill to the mid 60s after. That is the mock sensor's own doing: its startup code resets the simulation to 65 %. Whether a cell's *memory* survives a restart is a bigger question than this cell can answer, and it is the subject of Part 2.

---

## What Have You Learned

- A runtime carries **tags**, some automatic (`linux`, `@<id>`), some given at start (`--tag`). A cell **requires** tags with `-t`, and is only placed on a runtime that carries all of them.
- A requirement pins a cell to hardware. The sensor lives on `node1` because the probe does; no other node ever qualifies.
- `--policy always` makes the swarm restart a cell that stopped for any reason other than being removed. If no qualifying runtime exists, it waits; when one appears, the cell is back within a few seconds.
- `myrmic cells` tells you where each cell runs; `myrmic network status` translates the runtime id to a name; `myrmic runtimes list` gives you the process id to kill.

Next: [Part 2 - The Grow-Bed and Its State](./02_the-grow-bed-and-its-state.md), where the cell can move - and its memory may not follow.
