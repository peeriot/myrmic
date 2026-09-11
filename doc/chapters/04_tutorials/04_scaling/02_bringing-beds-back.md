# Part 2 - Bringing Beds Back

This is Part 2 of the [Scaling Your Application](../04_scaling.md) tutorial. You kill the node a bed runs on and teach the bed root to bring the bed back; then you kill the node the root runs on and see what that does to the beds - **learning how a parent is told about a lost child, and how it rebuilds its tree after its own restart.**

It continues where [Part 1](./01_roots-that-spawn.md) left off: four runtimes, the root and three beds running, Terminal 2 on `myrmic subscribe bed_state`.

---

## Step 6 - Copies First

You are about to kill nodes, so remember the lesson of the [Resilience](../03_resilience.md) tutorial before you do: a cell's state has one copy by default, on a node the swarm picks. That is true for the beds' moisture, and it is true for the bed root's list of beds. Ask for copies on every node - all four carry the automatic `linux` tag - and give the swarm half a minute to make them:

```bash
myrmic replicate app:greenhouse -t linux
```

```text
TARGET                                              TAGS
app:greenhouse                                      linux
scope:CELLS/72fdd5d5-ca6b-54af-b017-c624f8b8b1d7/p  @d40df21caca94c98be184fee8a7645d3 (provisional)
scope:CELLS/777e343f-cc44-5964-850d-9302dbd591ee/p  @4f741c9b645144daa3d0e8776399c7bf (provisional)
...
```

The first row is your entry; the `provisional` rows below it are the single copies the swarm had placed on its own, one per cell, which the entry now supersedes. The entry covers every cell of the application, the beds that exist and the beds the root spawns later. Check the root's data once the copies are in place:

```bash
myrmic database status bed-root --active
```

```text
INFO  probed; collecting answers for up to 10s (stops 2s after the last)...
CELLS/ec2709f8-5e11-5ca8-a152-5d195d73a00a/p  in sync  4 node(s)
  112a7f3f  full       6 head(s), latest 2026-09-08T15:04:16.311905247Z
  4f741c9b  full       6 head(s), latest 2026-09-08T15:04:16.311905247Z
  ca157d28  full       6 head(s), latest 2026-09-08T15:04:16.311905247Z
  d40df21c  full       6 head(s), latest 2026-09-08T15:04:16.311905247Z
```

Without this step, the rest of the part would still *run* - but the beds would forget their moisture, and in Step 9 the restarted root would find an empty list, because the only copy of it died with its node.

---

## Step 7 - A Bed Is Lost

What happens when the node a bed runs on dies? The swarm notices, after about a minute, that the node has stopped renewing its lease. It removes the bed from the registry, and it sends the bed's **parent** a notification: `cell_lost`, with the child's name and the reason. Then it does nothing else. A spawned cell has no restart policy - you saw the `—` in Part 1 - so whether the bed comes back is entirely the parent's decision. In Part 1's root that decision is not made at all: a cell that declares no handler for the notification simply drops it.

Fifteen lines fix that. Add one import at the top of `bed-root/src/lib.rs`:

```rust
use myrmic_sdk::monitor::{CellLost, LostReason};
```

and this handler at the end of the file:

```rust
/// The swarm tells a parent when one of its children is gone.
#[myrmic_sdk::monitor]
fn child_lost(_md: Metadata, loss: CellLost) -> Result<()> {
    // Only losses worth repairing: the node died, or the bed crashed.
    if !matches!(loss.reason, LostReason::NodeLost | LostReason::Crashed) {
        return Ok(());
    }

    let beds = BEDS.load()?.unwrap_or_default();
    let name = loss.local_name.as_deref().unwrap_or("");
    let Some(bed) = beds.iter().find(|b| format!("bed-{}", b.index) == name) else {
        return Ok(());
    };

    myrmic_sdk::warn!("{name} lost; respawning in '{}'", bed.sector)?;
    if let Err(err) = spawn_bed(bed.index, &bed.sector) {
        myrmic_sdk::warn!("{name} could not be respawned: {}", <&str>::from(err))?;
    }

    Ok(())
}
```

`child_lost`, marked `#[myrmic_sdk::monitor]`, is the handler the swarm calls with the notification. It looks the lost bed up in the list by name and calls the `spawn_bed` you wrote in Part 1, with the same name and the same sector - the same identity, so the bed finds its state.

Not every loss deserves a respawn. `NodeLost` and `Crashed` do. `Terminated` means the root or an operator removed the bed on purpose; `Stopped` means the bed ended itself; `SpawnFailed` means it never came up, so spawning again would fail again. The handler returns `Ok` for those.

One rule about the name: the function may be called anything except `on_cell_lost`, which is used internally.

Deploy the new root. A running application is not modified in place, so remove it first:

```bash
myrmic delete greenhouse --app
myrmic deploy app_specs.yml
myrmic cells
```

If the deploy answers `class 'bed-root' has active instances`, the removal is still settling; wait a few seconds and run it again.

```text
  cell      sri                                   kind  runtime     age  policy  class     srn
──── greenhouse ───────────────────────────────────────────────────────────────────────────────────
  bed-root  ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  2s   always  bed-root  bed-root
```

The beds are gone: deleting the application removed them, and this root does not create beds on its own. Ask for one in sector B:

```bash
myrmic send bed-root add_bed '"sectorB"'
myrmic cells
```

```text
  cell      sri                                   kind  runtime     age  policy  class     srn
──── greenhouse ─────────────────────────────────────────────────────────────────────────────────────────
  bed-root  ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  6s   always  bed-root  bed-root
  └─ bed-4  22bffd66-a421-5ddb-8f3e-6530c8fd010a  wasm  [4]f741c9b  4s   —       grow-bed  bed-root/bed-4
```

`bed-4`, not `bed-1`. Deleting the application removed the cells, not their data: the new root has the same identity as the old one, so it found the list `[1, 2, 3]` where the old one left it and continued counting. Keep that in mind for Step 9.

Now watch the root while you pull the plug on `bed-4`'s node. Cell logs are only kept while a retention period is set, so switch that on first. In Terminal 2:

```bash
myrmic telemetry set-db-retention 1h
myrmic telemetry debug --id bed-root
```

In Terminal 1, `bed-4` runs on `[4]f741c9b`, which is `node3` in the table from Step 1. Find its process id and kill it:

```bash
myrmic runtimes list
kill -9 <pid of node3>
```

For about a minute nothing happens; `myrmic cells` still shows `bed-4` on `node3`. Then, in Terminal 2:

```text
[2026-09-08T15:06:54.298000000Z] COMMAND=__sys_cell_lost receiver_sri=ec2709f8-5e11-5ca8-a152-5d195d73a00a, payload=0x1022bffd66...
[2026-09-08T15:06:54.300313464Z] WARN bed-4 lost; respawning in 'sectorB'
```

The command's row is still in `bed-root`'s mailbox while the stream reads it, which is why the `COMMAND` line shows up here. A command a handler has already consumed appears through that handler's log lines instead.

In Terminal 1:

```bash
myrmic cells
```

```text
  cell      sri                                   kind  runtime     age  policy  class     srn
──── greenhouse ─────────────────────────────────────────────────────────────────────────────────────────
  bed-root  ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  1m   always  bed-root  bed-root
  └─ bed-4  22bffd66-a421-5ddb-8f3e-6530c8fd010a  wasm  [d]40df21c  6s   —       grow-bed  bed-root/bed-4
```

Same name, same SRI, a fresh `age`, and a new home: `[d]40df21c` is `node4`, the other sector B node. The notification took 64 seconds to arrive - the time the swarm gives a silent node before declaring it dead - and the respawn took a second. Bring `node3` back so that both sectors are whole again:

```bash
myrmic runtimes start -n node3 --tag sectorB --detached
```

---

## Step 8 - The Root Is Lost

The root runs on `node1`. What if *that* node dies? The root is a root with `restart: always`, so the swarm brings it back. But its beds are children of the root that died. Kill `node1` and keep `myrmic cells` running every ten seconds:

```bash
myrmic runtimes list
kill -9 <pid of node1>
```

```text
  cell      sri                                   kind  runtime     age  policy  class     srn
──── greenhouse ─────────────────────────────────────────────────────────────────────────────────────────
  bed-root  ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  3m   always  bed-root  bed-root
  └─ bed-4  22bffd66-a421-5ddb-8f3e-6530c8fd010a  wasm  [d]40df21c  2m   —       grow-bed  bed-root/bed-4
```

```text
  cell      sri                                   kind  runtime     age  policy  class     srn
──── greenhouse ───────────────────────────────────────────────────────────────────────────────────
  bed-root  ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [4]f741c9b  9s   always  bed-root  bed-root
```

The root is back, on `node3`, and the tree under it is empty. `bed-4` was on `node4`, a perfectly healthy node, and it is gone anyway. Two things happened, in this order:

1. **The bed was stopped by its own runtime**, about fifty seconds after the kill and *before* the root was redeployed. A child belongs to one incarnation of its parent, and its runtime keeps checking that the parent is alive. When the parent's node had been silent for longer than the grace period, the runtime stopped the bed - a child does not outlive its parent. Had the parent come back sooner, the outcome would have been the same: the runtime also stops a child whose parent has been replaced by a new incarnation.
2. **Nobody was told.** The root that could have received a `cell_lost` for `bed-4` was dead itself, and the new root never knew `bed-4` existed. The monitor handler is for a child lost while its parent lives; it does not run here.

So a root restart takes the whole tree down with it, and the monitor handler alone does not build it back up. The new root still knows which beds existed, though: the list is in its state, and the state survived.

Bring `node1` back before the next step:

```bash
myrmic runtimes start -n node1 --tag sectorA --detached
```

---

## Step 9 - Rebuild From the List

The root already remembers every bed it created. All it has to do when it starts is make sure each one is running. Two more imports at the top of `bed-root/src/lib.rs`:

```rust
use core::time::Duration;

use myrmic_sdk::{Callback, ClassHandle, Metadata, Result, SpawnError, String, Vec, format};
```

then this block after `spawn_bed`:

```rust
/// How long to wait before trying again when a bed could not be spawned.
const RETRY: Duration = Duration::from_secs(20);

/// Runs `ensure` again after `RETRY`.
fn retry_later() -> Result<()> {
    let _ = myrmic_sdk::delay(Callback::of::<ensure>(), RETRY)
        .build()
        .map_err(|_| "timer failed")?;

    Ok(())
}

/// Runs on every start of the root, the first deploy and every restart.
/// Spawning is deploying and takes a moment, and the root is not up until
/// `init` returns - so the rebuild is scheduled, not done here.
#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    let _ = myrmic_sdk::delay(Callback::of::<ensure>(), Duration::from_secs(1))
        .build()
        .map_err(|_| "timer failed")?;

    Ok(())
}

/// Timer target: makes sure every bed in the list is running. A bed that
/// already runs is fine; a bed that cannot be spawned right now is tried
/// again later.
#[myrmic_sdk::cmd]
fn ensure(_md: Metadata) -> Result<()> {
    myrmic_sdk::info!("ensure: running")?;
    let mut missing = false;

    for bed in BEDS.load()?.unwrap_or_default() {
        match spawn_bed(bed.index, &bed.sector) {
            Ok(()) => myrmic_sdk::info!("bed-{} spawned", bed.index)?,
            Err(SpawnError::AlreadyExists) => {
                myrmic_sdk::info!("bed-{} already running", bed.index)?
            }
            Err(err) => {
                myrmic_sdk::warn!("bed-{} not running: {}", bed.index, <&str>::from(err))?;
                missing = true;
            }
        }
    }

    if missing {
        retry_later()?;
    }

    Ok(())
}
```

and one line in `child_lost`, right after the warning that a bed could not be respawned:

```rust
        retry_later()?;
```

`ensure` walks the list and spawns every bed. A bed that already runs answers `AlreadyExists`, which is treated as success, so calling it twice is harmless. A bed that cannot be spawned right now - its whole sector is down, say, or the swarm is still cleaning up after a lost node - is reported, and `retry_later` schedules another `ensure` twenty seconds later. The monitor handler falls back to the same retry, so from now on no refused respawn is final: the root keeps trying, one attempt every twenty seconds, until every bed in the list is running; then the timer is simply not re-armed.

`init` runs on every start of the root - the first deploy and every restart - and it does *not* run the loop directly. Spawning is deploying, and a deploy takes a moment; the root itself does not count as deployed until `init` returns. Spawning in `init` would make the root's own deploy wait for its children's deploys, and time out. So `init` schedules the `ensure` command one second later, and returns at once.

Deploy the new root, and look at the tree a few seconds later:

```bash
myrmic delete greenhouse --app
myrmic deploy app_specs.yml
myrmic cells
```

```text
  cell      sri                                   kind  runtime     age  policy  class     srn
──── greenhouse ─────────────────────────────────────────────────────────────────────────────────────────
  bed-root  ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  8s   always  bed-root  bed-root
  ├─ bed-1  777e343f-cc44-5964-850d-9302dbd591ee  wasm  [c]a157d28  7s   —       grow-bed  bed-root/bed-1
  ├─ bed-2  f1356c6d-8603-5fa5-a659-35764d15a3bf  wasm  [4]f741c9b  6s   —       grow-bed  bed-root/bed-2
  ├─ bed-3  72fdd5d5-ca6b-54af-b017-c624f8b8b1d7  wasm  [1]12a7f3f  6s   —       grow-bed  bed-root/bed-3
  └─ bed-4  22bffd66-a421-5ddb-8f3e-6530c8fd010a  wasm  [d]40df21c  6s   —       grow-bed  bed-root/bed-4
```

All four beds, one second after the root, without a single `add_bed`. The list said four, so there are four - `bed-1` to `bed-3` included, which have not run since Step 7. The layout of your greenhouse is not in `app_specs.yml`; it is in the root's state.

Give the beds a value to remember and the copies half a minute to catch up. Then look up which node hosts `bed-root` now - `node1` in the table above, but yours may differ - and kill it, with Terminal 2 freshly started on `myrmic telemetry debug --id bed-root`:

```bash
myrmic publish moisture 55.0
```

```bash
myrmic runtimes list
kill -9 <pid of the node hosting bed-root>
```

A minute passes. Then Terminal 2 - if it stays silent once the root has moved, stop the debug stream and start it again:

```text
[2026-09-08T15:17:02.057626945Z] INFO ensure: running
[2026-09-08T15:17:02.102211980Z] INFO bed-1 spawned
[2026-09-08T15:17:02.143541072Z] INFO bed-2 spawned
[2026-09-08T15:17:02.184028208Z] INFO bed-3 spawned
[2026-09-08T15:17:02.226429182Z] INFO bed-4 spawned
```

and Terminal 1:

```text
  cell      sri                                   kind  runtime     age  policy  class     srn
──── greenhouse ─────────────────────────────────────────────────────────────────────────────────────────
  bed-root  ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [4]f741c9b  5s   always  bed-root  bed-root
  ├─ bed-1  777e343f-cc44-5964-850d-9302dbd591ee  wasm  [c]a157d28  3s   —       grow-bed  bed-root/bed-1
  ├─ bed-2  f1356c6d-8603-5fa5-a659-35764d15a3bf  wasm  [d]40df21c  3s   —       grow-bed  bed-root/bed-2
  ├─ bed-3  72fdd5d5-ca6b-54af-b017-c624f8b8b1d7  wasm  [c]a157d28  3s   —       grow-bed  bed-root/bed-3
  └─ bed-4  22bffd66-a421-5ddb-8f3e-6530c8fd010a  wasm  [4]f741c9b  3s   —       grow-bed  bed-root/bed-4
```

The root is on `node3`, every bed is back with a fresh `age` - including `bed-1` and `bed-3`, which ran on healthy nodes and were stopped with their old parent - and each bed stayed in its sector. Had Terminal 2 been on `myrmic subscribe bed_state` instead, it would have shown each bed announce itself as it started:

```text
[2026-09-08T15:17:02.099Z] event=bed_state sender=777e343f-cc44-5964-850d-9302dbd591ee payload=27 bytes
{
  "index": 1,
  "moisture": 55.0
}
```

The body is seconds old; the memory is from before the kill, read from one of the copies you asked for in Step 6.

Bring the killed node back, with its own name and tag:

```bash
myrmic runtimes start -n node1 --tag sectorA --detached
```

---

## What Have You Learned

- **A lost child is reported, not restarted.** The swarm sends the parent a `cell_lost` with the child's name and a reason; a `#[monitor]` handler decides. Respawn for `NodeLost` and `Crashed`; leave `Terminated`, `Stopped` and `SpawnFailed` alone.
- **A child does not outlive its parent.** When the root's node dies, its runtime-hosted children are stopped within about a minute, on healthy nodes too, and no notification reaches anyone. The monitor handler cannot cover this case.
- **The list rebuilds the tree.** `ensure` turns the root's list into running cells idempotently, and `init` schedules it on every start. Deleting and redeploying the application, or losing the root's node, both end with the same tree.
- **A refused respawn is not final.** `ensure` and the monitor handler both fall back to a timer, so a bed whose sector is down comes back when the sector does.
- **Never spawn in `init`.** A deploy waits for `init`; `init` must not wait for deploys. Schedule the work one second later.
- **Copies first.** The root's list and the beds' moisture are cell state with one copy by default. `myrmic replicate` is what made the restarted root find its list and the respawned beds find their readings.
- **Expect about a minute** for a dead node to be given up, then a second for the tree to be back.

Next: [Part 3 - One Sensor per Bed](./03_one-sensor-per-bed.md), where every bed gets a sensor of its own.
