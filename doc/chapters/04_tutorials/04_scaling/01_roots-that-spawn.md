# Part 1 - A Root That Spawns

This is Part 1 of the [Scaling Your Application](../04_scaling.md) tutorial. You build a grow-bed that knows nothing but its own state, and a **bed root** that creates grow-beds on request, in the sector of the greenhouse you name. You spawn three beds from the CLI and see them as a tree in `myrmic cells` - **learning how one cell creates another, and where the new cell runs.**

---

## Step 1 - Start Four Runtimes

Picture a greenhouse with two sectors, each with two computers. In Terminal 1:

```bash
myrmic runtimes start -n node1 --tag sectorA --detached
myrmic runtimes start -n node2 --tag sectorA --detached
myrmic runtimes start -n node3 --tag sectorB --detached
myrmic runtimes start -n node4 --tag sectorB --detached
```

Give them a few seconds to find each other:

```bash
myrmic network status
```

```text
Discovered 4 node(s)

  #  name   kind   id          tags
  0  node1  linux  [1]12a7f3f  sectorA, @112a7f3f15aa4f7ead00258a4c8c609a, linux
  1  node2  linux  [c]a157d28  sectorA, @ca157d28d8cf44689a97832a30327a8c, linux
  2  node3  linux  [4]f741c9b  sectorB, @4f741c9b645144daa3d0e8776399c7bf, linux
  3  node4  linux  [d]40df21c  sectorB, @d40df21caca94c98be184fee8a7645d3, linux
```

---

## Step 2 - A New Workspace

The cells of this tutorial start as simplified copies of the Smart Greenhouse cells and grow part by part, so they get a directory of their own:

```bash
mkdir scaling && cd scaling
myrmic new grow-bed
myrmic new bed-root
```

---

## Step 3 - The Grow-Bed

The grow-bed is the asset from the Smart Greenhouse, reduced to what this part needs: a state and one event handler. Replace the content of `grow-bed/src/lib.rs` with:

```rust
//! Grow-bed asset: the canonical state of one bed of plants.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, publish};

/// What the bed knows - also the payload of the `bed_state` event.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct Bed {
    index: u32,
    moisture: f32,
}

const BED: State<Bed> = State::new_const("bed");

/// The bed root passes the bed's index when it spawns us.
#[myrmic_sdk::init]
fn init(_md: Metadata, index: u32) -> Result<()> {
    let bed = BED.load()?.unwrap_or(Bed {
        index,
        moisture: 0.0,
    });
    BED.save(&bed)?;

    publish("bed_state", &bed)
}

/// A new reading: remember it and announce the bed's state.
#[myrmic_sdk::evt]
fn moisture(_md: Metadata, value: f32) -> Result<()> {
    let mut bed = BED.load()?.ok_or("bed not initialised")?;
    bed.moisture = value;
    BED.save(&bed)?;

    publish("bed_state", &bed)
}
```

One thing is new compared with the Smart Greenhouse grow-bed: `init` takes an argument. A cell that is *spawned* can be handed a value by the cell that spawns it, and the runtime decodes it into the type the handler declares - here a `u32`, the bed's index. The bed keeps it in its state so that its `bed_state` events say which bed is talking.

The rest you know. `BED` is the bed's state, `moisture` is the handler for the sensor's event, and every change is announced on `bed_state`.

---

## Step 4 - The Bed Root

So far every cell you deployed was a **root**: one line under `instances` in `app_specs.yml`, deployed by the CLI. Adding a bed meant editing the file and deploying again. The bed root moves that job into the swarm: it creates a grow-bed whenever it is asked to, in the sector it is asked for. Replace the content of `bed-root/src/lib.rs` with:

```rust
//! Bed root: creates one grow-bed child per bed.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{ClassHandle, Metadata, Result, SpawnError, String, Vec, format};

/// The class to spawn beds from, resolved when the application is deployed.
const GROW_BED: ClassHandle = myrmic_sdk::declare!("grow-bed");

/// One bed this root has created: its index and the sector it runs in.
#[derive(serde::Serialize, serde::Deserialize)]
struct Bed {
    index: u32,
    sector: String,
}

/// The beds this root has created.
const BEDS: State<Vec<Bed>> = State::new_const("beds");

/// Spawns `bed-<index>` on a node of `sector`.
fn spawn_bed(index: u32, sector: &str) -> core::result::Result<(), SpawnError> {
    GROW_BED
        .new()
        .name(format!("bed-{index}"))
        .tag(sector)
        .payload(&index)
        .spawn()
        .map(|_| ())
}

/// Creates the next bed in a sector: `myrmic send bed-root add_bed '"sectorA"'`.
#[myrmic_sdk::cmd]
fn add_bed(_md: Metadata, sector: String) -> Result<()> {
    let mut beds = BEDS.load()?.unwrap_or_default();
    let index = beds.last().map_or(1, |last| last.index + 1);

    // A failed handler is delivered again; a refused spawn should not be retried.
    if let Err(err) = spawn_bed(index, &sector) {
        myrmic_sdk::warn!("bed-{index} in '{sector}' refused: {}", <&str>::from(err))?;
        return Ok(());
    }

    beds.push(Bed { index, sector });
    BEDS.save(&beds)?;
    myrmic_sdk::info!("added bed-{index}")?;

    Ok(())
}
```

Three lines carry the idea.

`declare!("grow-bed")` names the **class** to create beds from. A class is a deployed Wasm binary; the application registers it under the id `grow-bed` in the next step, and `myrmic deploy` patches the class's identity into the bed root's binary when the application is deployed. The root never contains the bed's code.

`spawn_bed` creates one bed with `GROW_BED.new().name(...).tag(...).payload(...).spawn()`. `.name("bed-1")` gives the child a fixed name, and the child's identity is derived from its parent's identity and that name - `bed-root/bed-1` is the same cell every time, which matters in the next part. `.tag(sector)` is the placement requirement you know from the Resilience tutorial, now decided at runtime from the command's argument: the bed runs only on a node carrying that tag. `.payload(&index)` is the value the bed's `init` receives. The helper is worth its own function because the next part calls it from more than one place.

`BEDS` is the root's memory: for every bed it created, the index and the sector. It is ordinary cell state, one small list, and it is what gives the next bed the next number.

One habit to pick up right away: when the spawn fails, the handler logs a warning and returns `Ok`. A handler that returns an error is *not consumed* - the command is delivered again - and retrying a spawn that was just refused would not help.

---

## Step 5 - Deploy and Grow

Create `scaling/app_specs.yml`:

```yaml
name: greenhouse

classes:
  - id: bed-root
    build: ./bed-root
  - id: grow-bed
    build: ./grow-bed

instances:
  - class: bed-root
    restart: always
```

`grow-bed` appears under `classes` and not under `instances`: the application registers the class so that it can be spawned from, and deploys no bed of its own. The root gets `restart: always`, as every root in a real deployment should - if its runtime dies, the swarm brings it back. Deploy:

```bash
myrmic deploy app_specs.yml
myrmic cells
```

```text
  cell      sri                                   kind  runtime     age  policy  class     srn
──── greenhouse ───────────────────────────────────────────────────────────────────────────────────
  bed-root  ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  2s   always  bed-root  bed-root
```

One root, no beds. The root has no tag requirement, so it landed wherever the swarm chose - `node1` here. Ask for three beds, two in sector A and one in sector B:

```bash
myrmic send bed-root add_bed '"sectorA"'
myrmic send bed-root add_bed '"sectorB"'
myrmic send bed-root add_bed '"sectorA"'
myrmic cells
```

```text
  cell      sri                                   kind  runtime     age  policy  class     srn
──── greenhouse ─────────────────────────────────────────────────────────────────────────────────────────
  bed-root  ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  3s   always  bed-root  bed-root
  ├─ bed-1  777e343f-cc44-5964-850d-9302dbd591ee  wasm  [c]a157d28  3s   —       grow-bed  bed-root/bed-1
  ├─ bed-2  f1356c6d-8603-5fa5-a659-35764d15a3bf  wasm  [4]f741c9b  3s   —       grow-bed  bed-root/bed-2
  └─ bed-3  72fdd5d5-ca6b-54af-b017-c624f8b8b1d7  wasm  [1]12a7f3f  3s   —       grow-bed  bed-root/bed-3
```

`myrmic cells` draws a tree now. The beds hang under the root, and their SRNs are paths: `bed-root/bed-1`. Read the `runtime` column against the node table from Step 1: `bed-1` and `bed-3` asked for `sectorA` and sit on `node2` and `node1`; `bed-2` asked for `sectorB` and sits on `node3`. Your split inside a sector may differ; a bed never crosses into the other sector. And compare the two `policy` columns: the root says `always`, the beds say `—`. A spawned cell has no restart policy of its own - what happens to it when things go wrong is its parent's business, and a later part's subject.

Ask for a bed in a sector that does not exist:

```bash
myrmic send bed-root add_bed '"sectorC"'
myrmic cells
```

Nothing changes: no node carries `sectorC`, the spawn is refused, the root logs a warning, and the list stays at three. The command was consumed all the same - that is the `return Ok(())` in the handler at work.

Now give the beds something to remember. In Terminal 2, listen to them; in Terminal 1, play the sensor by hand:

```bash
myrmic subscribe bed_state
```

```bash
myrmic publish moisture 42.5
```

```text
[2026-09-08T14:00:15.112Z] event=bed_state sender=f1356c6d-8603-5fa5-a659-35764d15a3bf payload=27 bytes
{
  "index": 2,
  "moisture": 42.5
}

[2026-09-08T14:00:15.113Z] event=bed_state sender=777e343f-cc44-5964-850d-9302dbd591ee payload=27 bytes
{
  "index": 1,
  "moisture": 42.5
}

[2026-09-08T14:00:15.113Z] event=bed_state sender=72fdd5d5-ca6b-54af-b017-c624f8b8b1d7 payload=27 bytes
{
  "index": 3,
  "moisture": 42.5
}
```

Terminal 2 may first show a few older `bed_state` events - the ones the beds published when they started, with `"moisture": 0.0` - and on a fresh swarm the new readings can take up to half a minute to arrive while the storage for a brand-new event settles. If nothing comes, publish once more.

All three beds answered, on three different nodes, and all three now hold 42.5. That is how events work: `moisture` is delivered to every cell with a `moisture` handler, wherever it runs. Each bed has its own state - the `sender` values are the beds' identities from the table above - but they all heard the same reading. In a real greenhouse each bed has its own sensor, and a bed should listen to its own. That is the next part's problem.

---

## What Have You Learned

- A cell can **spawn** another cell from a class the application registered. `declare!` names the class, `.name()` fixes the child's identity, `.tag()` decides where it may run, `.payload()` hands it a value for `init`.
- A placement tag can come from **user input at runtime**. The spec never said where the beds go; the command did.
- A spawned cell shows up **under its parent** in `myrmic cells`, with a path for an SRN and no restart policy of its own.
- The root **remembers what it created** in ordinary state; the platform does not keep that list for it.
- A handler that returns an error is **delivered again** - return `Ok` after logging a failure that a retry cannot fix.
- Events are delivered **by name**, to every cell that handles them, across nodes. Three beds, one reading.

Next: [Part 2 - Bringing Beds Back](./02_bringing-beds-back.md), where nodes start dying.
