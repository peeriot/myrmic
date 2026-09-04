# Part 2 - The Pump

This is Part 2 of the [Smart Greenhouse](../02_smart-greenhouse.md) tutorial. You build the greenhouse's first actuator - the irrigation pump - and learn about *cell commands*: how a cell exposes them, and how to send them from the CLI.

It continues where [Part 1](./01_the-mock-sensor.md) left off: the runtime is running in Terminal 1, the moisture sensor is deployed and publishing, and Terminal 3 is subscribed to its readings.

---

## Step 4 - Create the Pump

So far the greenhouse can only *observe*. To act on the world - to actually water the bed - it needs an actuator. Ours is a pump, and it introduces the second messaging primitive of Myrmic.

Events, as you saw in Part 1, are broadcasts: the publisher does not know who listens. A pump is the opposite situation. "Turn on" is not an announcement to whoever cares - it is a request addressed to *one specific cell*, and exactly that cell should act on it. For this, Myrmic has **commands**.

### Expose commands with `#[cmd]`

In Terminal 2, from the `greenhouse/` directory, scaffold the second cell:

```bash
myrmic new pump
```

Then replace the content of `pump/src/lib.rs` with:

```rust
//! Pump adapter: `start` and `stop` drive the irrigation motor, and the
//! current state is announced on the `pump_state` event. The pump knows
//! nothing about plants or watering policy.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, publish};

const RUNNING: State<bool> = State::new_const("running");

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    RUNNING.save(&false)?;
    publish("pump_state", &false)
}

#[myrmic_sdk::cmd]
fn start(md: Metadata) -> Result<()> {
    RUNNING.save(&true)?;
    let _ = myrmic_sdk::info!("pump started (sender={:?})", md.sender).ok();
    publish("pump_state", &true)
}

#[myrmic_sdk::cmd]
fn stop(md: Metadata) -> Result<()> {
    RUNNING.save(&false)?;
    let _ = myrmic_sdk::info!("pump stopped (sender={:?})", md.sender).ok();
    publish("pump_state", &false)
}
```

The new pieces:

- `#[myrmic_sdk::cmd]` - marks a command handler, and the naming rule is the same one you learned for events: **the name of the function is the name of the command**. This cell answers to `start` and `stop`, and nothing else. Where an `#[evt]` handler receives whatever anyone broadcasts, a `#[cmd]` handler runs only when someone addresses *this* cell directly. The [Commands](../../05_guides/02_commands.md) guide covers the model in depth.
- `md.sender` - every handler receives `Metadata`, and for a command the sender identifies who asked. The pump logs it with `myrmic_sdk::info!`; you will see those log lines in a moment.
- `publish("pump_state", ...)` - a good adapter announces what it does. The pump does not know who cares that it started - it just says so, on `start`, on `stop`, and once at `init` so the initial state is on the network too. This is the two primitives working together: commands go in, events come out.

Notice what the pump does *not* contain: no moisture, no thresholds, no plants. An actuator adapter does what it is told and reports its state - deciding *when* to pump is someone else's job, and that separation is what will let us swap the decision-maker later without touching this cell.

Deploy it and check the swarm:

```bash
myrmic deploy pump
myrmic cells
```

Two cells now, each with its own `srn`: `moisture-sensor` and `pump`.

### Send commands via the CLI

You already know that the CLI can publish and subscribe to events like any cell. It can send commands too. First, make Terminal 3 watch the pump as well - restart the subscription with both events:

```bash
myrmic subscribe moisture,pump_state
```

Now, from Terminal 2, command the pump:

```bash
myrmic send pump start
```

`myrmic send` takes the target cell and the command name. In Terminal 3 the pump announces itself:

```text
[2026-08-26T17:02:11.482Z] event=pump_state sender=4e9ba24d-... payload=4 bytes
true
```

And the pump logged who asked. Look for it in the runtime logs:

```bash
myrmic telemetry logs
```

```text
INFO  [...] | trace_id = <id> | pump started (sender=Sri(<your cli session>))
```

That `sender` is your CLI session - at the moment, it is labeled as `external`.

You can now start and stop the pump on command. In a real greenhouse, that watering would show as an increase of moisture, by way of actual water and actual soil; in ours, the weather swing stands in for all of the physics. Either way, someone still has to decide *when* to water - and right now, that someone is you. Hold that thought: it is exactly the job we will automate.

---

## What Have You Learned

- Myrmic has two messaging primitives: *events* are broadcasts to whoever listens; *commands* are requests addressed to one specific cell.
- `#[cmd]` exposes a command handler, and the name of the function is the name of the command - the same rule events follow.
- `md.sender` tells a command handler who asked; commands are never anonymous. `myrmic_sdk::info!` log lines show up in `myrmic telemetry logs`.
- The CLI sends commands with `myrmic send <cell> <command>` - alongside `publish` and `subscribe`, it can act as any side of a conversation in the swarm.
- A good adapter cell is dumb on purpose: commands go in, state events come out, and no application logic lives inside.

## Next Step

The sensor measures, the pump waters, and you are the only thing connecting them. Before we automate that decision, the application needs one place that knows the *state of the bed* as a whole. In [Part 3 - The Grow-Bed](./03_the-grow-bed.md) we build it: the **Asset Cell**.
