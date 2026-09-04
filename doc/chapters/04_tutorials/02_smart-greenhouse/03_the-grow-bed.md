# Part 3 - The Grow-Bed

This is Part 3 of the [Smart Greenhouse](../02_smart-greenhouse.md) tutorial. You build the **Asset Cell** that owns the canonical state of the bed - learning how to model structured state and share it on the network.

It continues where [Part 2](./02_the-pump.md) left off: the runtime is running in Terminal 1, the sensor and the pump are deployed, and Terminal 3 is your event window.

---

## Step 5 - Create the Grow-Bed

The swarm now has two devices talking - and a question nobody can answer: *how is grow bed 1 doing?* The sensor knows a number, the pump knows a motor; neither knows the bed. This is what the **Asset** pattern is for: one cell owns the canonical state of one real-world thing. Everyone who wants to know about the bed asks the bed - never the sensor. That indirection is what lets you later swap the mock for real hardware, or average three sensors into one bed, without touching anything downstream. The [cell patterns](../../06_concepts/07_cell-patterns.md) concept page describes the full family.

Scaffold the third cell:

```bash
myrmic new grow-bed
```

Replace the content of `grow-bed/src/lib.rs` with:

```rust
//! Grow-bed asset: owns the canonical state of one bed of plants - the latest
//! moisture reading, the pump status, and the moisture range the plants want.
//! Every change is announced on the `bed_state` event. It commands nothing:
//! actuation belongs to the pump adapter, decisions to the irrigation agent.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, publish};

/// Canonical state of the bed - also the payload of the `bed_state` event.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct Bed {
    moisture: f32,
    pump_on: bool,
    target_low: f32,
    target_high: f32,
}

impl Default for Bed {
    fn default() -> Self {
        Self {
            moisture: 0.0,
            pump_on: false,
            target_low: 55.0,
            target_high: 75.0,
        }
    }
}

const BED: State<Bed> = State::new_const("bed");

/// The moisture range the plants in this bed want, settable at runtime.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct TargetRange {
    low: f32,
    high: f32,
}

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    // Seed the default only on first deploy; a redeployed bed keeps its state.
    if BED.load()?.is_none() {
        BED.save(&Bed::default())?;
    }
    Ok(())
}

/// A new sensor reading: update the canonical state and announce it.
#[myrmic_sdk::evt]
fn moisture(_md: Metadata, value: f32) -> Result<()> {
    let mut bed = BED.load()?.unwrap_or_default();
    bed.moisture = value;
    BED.save(&bed)?;
    publish("bed_state", &bed)
}

/// The pump announced a state change: record and announce it.
#[myrmic_sdk::evt]
fn pump_state(_md: Metadata, on: bool) -> Result<()> {
    let mut bed = BED.load()?.unwrap_or_default();
    bed.pump_on = on;
    BED.save(&bed)?;
    publish("bed_state", &bed)
}

/// Domain command: this bed now grows plants that want a different range.
#[myrmic_sdk::cmd]
fn set_target(_md: Metadata, range: TargetRange) -> Result<()> {
    let mut bed = BED.load()?.unwrap_or_default();
    bed.target_low = range.low;
    bed.target_high = range.high;
    BED.save(&bed)?;
    publish("bed_state", &bed)
}
```

Reading it top to bottom:

- `#[derive(..., myrmic_sdk::Message)]` - the `Message` derive gives a struct a wire encoding (JSON by default), so it can travel as an event or command payload. The [Message Encoding](../../05_guides/04_message-encoding.md) guide covers the codecs.
- `State<Bed>` - the same persistent state you know from Parts 1 and 2, now holding a struct. The [State and Storage](../../05_guides/06_state-and-storage.md) guide shows what else the cell database offers (key-value trees, tables).
- The two `#[evt]` handlers are where the asset earns its keep: it listens to the adapters, folds their raw signals into one canonical picture, and announces every change on `bed_state`. From here on, the raw `moisture` event is a detail between the sensor and the asset - everyone else reads `bed_state`.
- `set_target` is the bed's only command, and it is a *domain* command: it exists because the bed can be replanted with plants that want a different range. Watering is deliberately not here - a bed of plants does not water itself.

Deploy it and check the swarm:

```bash
myrmic deploy grow-bed
myrmic cells
```

Three cells now. Point Terminal 3 at the asset's announcements:

```bash
myrmic subscribe bed_state
```

Expected output (one per sensor tick - and structs arrive as readable JSON):

```text
[2026-08-26T17:51:06.487Z] event=bed_state sender=fd02ce9b-... payload=75 bytes
{
  "moisture": 63.399994,
  "pump_on": false,
  "target_high": 75.0,
  "target_low": 55.0
}
```

Now replant the bed. `myrmic send` accepts a JSON payload, and the cell decodes it straight into the typed `TargetRange`:

```bash
myrmic send grow-bed set_target '{"low": 70, "high": 85}'
```

Watch Terminal 3: the very next `bed_state` carries the new targets. One thing that changed, one place that knows it, everyone informed.

---

## What Have You Learned

- The **Asset** pattern: one cell owns the canonical state of one real-world thing. Consumers read the asset, not the device adapters - so devices can be swapped without touching anything downstream.
- `State<T>` holds structs as easily as scalars; struct state needs `serde` with `default-features = false`, because cells are `no_std`.
- The `myrmic_sdk::Message` derive gives a struct a wire encoding (JSON by default), so events and commands can carry structured payloads.
- `myrmic send` takes a JSON payload - `'{"low": 70, "high": 85}'` - and the receiving handler gets it as a typed struct.
- An asset folds raw device signals into one canonical picture and announces every change as an event of its own.

## Next Step

The bed now knows how it is doing and what it wants - but nobody acts on it yet. In [Part 4 - The Irrigation Agent](./04_the-irrigation-agent.md) we hire the decision-maker.
