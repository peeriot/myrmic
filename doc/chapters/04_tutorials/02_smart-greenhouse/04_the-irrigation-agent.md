# Part 4 - The Irrigation Agent

This is Part 4 of the [Smart Greenhouse](../02_smart-greenhouse.md) tutorial. You automate the watering decision - learning how cells send commands to each other, and where an application's business logic belongs.

It continues where [Part 3](./03_the-grow-bed.md) left off: the runtime is running in Terminal 1, the sensor, the pump, and the grow-bed are deployed, and Terminal 3 is your event window.

---

## Step 6 - Create the Irrigation Agent

Part 2 ended with a job offer: someone has to watch the moisture and drive the pump, and so far that someone is you. Time to delegate.

The **Agent** pattern is where decisions live: a cell that evaluates conditions and issues commands - and the *only* place in the application that does. The division of labor is now complete: sensors measure, assets remember, adapters act, agents decide. Keeping all business logic in one cell means you can later replace the watering policy - smarter scheduling, weather forecasts, machine learning - by swapping a single cell, while sensor, pump, and grow-bed stay untouched. The [cell patterns](../../06_concepts/07_cell-patterns.md) page describes the full family.

Our policy is deliberately simple, but it is a real controller's policy: **hysteresis**. Two thresholds instead of one - start watering when the bed's moisture drops below its low target, stop only when it climbs above the high one. The gap between them keeps the pump from chattering on and off around a single set point.

And note what the agent reads: the grow-bed's `bed_state` - never the raw sensor. The bed's state already carries the target range, so the agent stores almost nothing itself: just a flag remembering whether it is mid-watering.

Scaffold the cell and add the `serde` line to `irrigation-agent/Cargo.toml`, as in Part 3:

```bash
myrmic new irrigation-agent
```

```toml
serde = { version = "1", default-features = false, features = ["alloc", "derive"] }
```

Replace the content of `irrigation-agent/src/lib.rs` with:

```rust
//! Irrigation agent: the only cell that makes decisions. It reads the
//! grow-bed's canonical state - never the raw sensor - and drives the pump
//! adapter with hysteresis: start below the bed's low target, stop above the
//! high one, so the pump never chatters around a single set point.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, Sri, Void, publish, send};

const PUMP: &str = "pump";

/// Hysteresis flag: are we in the middle of a watering cycle?
const WATERING: State<bool> = State::new_const("watering");

/// Payload of the grow-bed's `bed_state` event.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct BedState {
    moisture: f32,
    pump_on: bool,
    target_low: f32,
    target_high: f32,
}

#[myrmic_sdk::evt]
fn bed_state(_md: Metadata, bed: BedState) -> Result<()> {
    let watering = WATERING.load()?.unwrap_or_default();
    if !watering && bed.moisture < bed.target_low {
        pump("start")?;
        WATERING.save(&true)?;
        publish("watering_started", &bed.moisture)?;
    } else if watering && bed.moisture > bed.target_high {
        pump("stop")?;
        WATERING.save(&false)?;
        publish("watering_stopped", &bed.moisture)?;
    }
    Ok(())
}

fn pump(command: &str) -> Result<()> {
    let pump = Sri::of_path(PUMP).map_err(|_| "invalid pump srn")?;
    send(pump, command, &Void)
}
```

Reading it top to bottom:

- The agent declares its *own* `BedState` struct matching the event's JSON fields - the same declaration the grow-bed has, deliberately duplicated. Cells share wire formats, not Rust types; each side owns its definition and can evolve independently.
- `send(sri, command, &payload)` - the cell-side counterpart of the `myrmic send` you used in Part 2. Any cell can command any other; the [Commands](../../05_guides/02_commands.md) guide covers the details.
- `Sri::of_path("pump")` - resolves a cell's SRN to its SRI, so the agent can address the pump by name.
- `Void` - the explicit "no payload", for commands like `start` that need no arguments.
- The agent also *publishes* - `watering_started` and `watering_stopped`. Decisions are announcements too: anyone (a logger, an alarm, a statistics cell) can react to them, and you can watch them from the CLI.

Deploy it:

```bash
myrmic deploy irrigation-agent
```

Now watch the handover of your job. Point Terminal 3 at the decision events:

```bash
myrmic subscribe bed_state,watering_started,watering_stopped
```

When the weather swing next takes the moisture below the bed's low target, the agent acts:

```text
[2026-08-26T18:05:30.557Z] event=watering_started sender=9f2172bd-... payload=8 bytes
54.99996
```

And from then on, the `bed_state` events show `"pump_on": true` - until the moisture climbs past the high target and a `watering_stopped` arrives. The cycle repeats, forever, with nobody at the keyboard.

One more look proves who is in charge now. In Part 2, the pump's log showed `external` as the sender. Check it again:

```bash
myrmic telemetry logs
```

```text
INFO  [...] | trace_id = <id> | pump started (sender=Sri(9f2172bd-...))
```

The sender is the irrigation agent's SRI. Same pump, same command, a different caller - and the pump never knew the difference.

---

## What Have You Learned

- The **Agent** pattern: all business logic lives in one cell - sensors measure, assets remember, adapters act, agents decide. Swap the agent and the policy changes; nothing else moves.
- Cells command each other with `send(sri, command, &payload)` - the cell-side counterpart of `myrmic send` - and `Sri::of_path` resolves a name to an address. `Void` is the explicit "no payload".
- Hysteresis: two thresholds with a gap keep an actuator from chattering around a single set point.
- Agents announce their decisions as events, so the rest of the swarm - and you, via `myrmic subscribe` - can follow along.
- `md.sender` told the story: the same `start` command, once from your CLI session, now from the agent - and the pump could not care less.

## Next Step

The greenhouse runs itself - but only terminal windows can see it. In [Part 5 - The Dashboard](./05_the-dashboard.md) we put the bed in the browser: a cell that serves a web page through the gateway.
