# Part 1 - The Mock Sensor

This is Part 1 of the [Smart Greenhouse](../02_smart-greenhouse.md) tutorial. You start the runtime, scaffold your first cell, and build a mock soil-moisture sensor that publishes a reading every second - and reacts to the weather. Along the way you will learn about *cell events*: how cells publish them and how they subscribe to them.

---

## Step 1 - Start the Runtime

Everything in Myrmic happens inside a *runtime*: the process that runs your cells, stores their state, and routes every message between them. On a real installation one runtime runs on every device of the swarm; today the whole swarm is one runtime on your computer.

Open Terminal 1, start it, and leave it running:

```bash
myrmic runtimes start
```

The runtime now occupies this terminal - that is fine, it is its job. Everything else happens elsewhere. Open Terminal 2, your working shell for the rest of the tutorial, and check that the runtime is alive:

```bash
myrmic runtimes list
```

Expected output:

```text
default	running	pid=<pid>
```

There it is: a running swarm of one machine, with nothing deployed yet. Let us give it something to run.

---

## Step 2 - Create the Workspace and Your First Cell

In Terminal 2, create a directory for the application and scaffold your first cell:

```bash
mkdir greenhouse && cd greenhouse
myrmic new moisture-sensor
```

Expected output:

```text
INFO  Creating 'moisture-sensor'
```

Take a moment to look at what `myrmic new` generated:

```text
greenhouse/
└── moisture-sensor/
    ├── Cargo.toml     -- the crate manifest
    ├── .gitignore
    └── src/
        └── lib.rs     -- the cell's code
```

A cell is nothing exotic: it is a small Rust crate that compiles to WebAssembly. Two things in the scaffold are Myrmic-specific:

- In `Cargo.toml`, the `[package.metadata.myrmic]` section configures the cell's memory (for now just `heap_size` - how much heap the cell gets at runtime), and the `myrmic-sdk` dependency provides the macros and host functions cells are built from.
- `src/lib.rs` contains a small counter demo - the same one the [Quickstart](../../01_quickstart.md) walks through. We will replace it entirely in the next step.

---

## Step 3 - Create the Mock Sensor

Our first cell simulates a soil-moisture sensor. A real greenhouse would read the value from hardware through the signal layer; here a small simulation stands in: the moisture swings between a dry bound and a wet bound, the way real soil follows the weather - dry spells and wet fronts taking turns. That distinction matters less than you might think: to the rest of the swarm the interface is identical either way - a `moisture` event, once per second.

### Publish a reading every second

Replace the content of `moisture-sensor/src/lib.rs` with:

```rust
//! Mock soil-moisture sensor: publishes a `moisture` reading every second.
//! The simulated moisture swings between a dry and a wet bound, the way real
//! soil follows the weather.
#![no_std]

use core::time::Duration;

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Callback, Metadata, Result, publish};

/// Simulated soil moisture, in percent.
const MOISTURE: State<f32> = State::new_const("moisture");

/// Direction of the swing: `true` while the simulated weather is wetting.
const RISING: State<bool> = State::new_const("rising");

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    MOISTURE.save(&65.0)?;
    // The handle could cancel the timer later; this sensor measures forever.
    let _ = myrmic_sdk::interval(Callback::of::<measure>(), Duration::from_secs(1))
        .build()
        .map_err(|_| "timer failed")?;
    Ok(())
}

/// Timer target: the simulation advances one tick, then the reading is
/// published. The value swings: it dries down to 35%, turns, wets up to 90%.
#[myrmic_sdk::cmd]
fn measure(_md: Metadata) -> Result<()> {
    let mut moisture = MOISTURE.load()?.unwrap_or(65.0);
    let mut rising = RISING.load()?.unwrap_or_default();
    if moisture <= 35.0 {
        rising = true;
    } else if moisture >= 90.0 {
        rising = false;
    }
    let delta = if rising { 1.2 } else { -0.4 };
    moisture = (moisture + delta).clamp(0.0, 100.0);
    MOISTURE.save(&moisture)?;
    RISING.save(&rising)?;
    publish("moisture", &moisture)
}
```

Reading it top to bottom:

- `State<f32>`, `State<bool>` - the cell's persistent memory. Cells can be restarted or moved to another device; whatever is in `State` survives, because it lives in the runtime's database, not in the Wasm module instance.
- `#[myrmic_sdk::init]` - runs when the cell is deployed. It seeds the moisture at 65% and schedules the measurements.
- `myrmic_sdk::interval(...)` - we have to emit one reading per second, and in Myrmic you do not block a cell with an endless loop: while a cell is handling a message, it cannot respond to another one. For anything periodic you use the runtime's scheduler instead - `interval` asks it to invoke the `measure` handler every second. `measure` is an ordinary `#[cmd]` handler; to the cell, the scheduler is just one more sender of commands. See the [Scheduling Handlers](../../05_guides/05_scheduling-handlers.md) guide for the details (one-shot timers, cancellation, and more).
- `publish("moisture", &moisture)` - broadcasts an event to the whole swarm. The sensor does not know or care who listens. That indifference is the core of the architecture: the next steps will add listeners without touching this cell again.

Deploy it (`myrmic deploy` builds and deploys in one step):

```bash
myrmic deploy moisture-sensor
```

Expected output:

```text
INFO  deploying cell (srn = moisture-sensor, sri = <uuid>)
INFO  deployed cell (srn = moisture-sensor, sri = <uuid>)
```

Ask the swarm what is running:

```bash
myrmic cells
```

The sensor cell appears, addressed by two identifiers: the `srn` (its human-readable name, derived from the crate name) and the `sri` (the unique id the runtime uses internally, derived from the srn). Right now the sensor cell is already measuring - once per second, into the void, because nobody is listening yet.

So let us listen. Open Terminal 3 and subscribe to the `moisture` event:

```bash
myrmic subscribe moisture
```

Expected output (a new reading every second, moisture slowly falling):

```text
INFO  subscribed to: moisture (ctrl-c to stop)

[2026-08-26T15:18:34.366Z] event=moisture sender=365d6cfe-... payload=9 bytes
64.6

[2026-08-26T15:18:35.367Z] event=moisture sender=365d6cfe-... payload=8 bytes
64.199997
```

Terminal 3 is now your window into the application - leave it running for the rest of the tutorial. The sensor works; publishing is half of the story.

### Subscribe to an event

Cells do not only publish events - they can react to them too. Weather exists, so let us teach the sensor about rain. Add this handler at the end of `moisture-sensor/src/lib.rs`:

```rust
/// A rain shower adds a one-time amount of moisture.
#[myrmic_sdk::evt]
fn rain(_md: Metadata, amount: f32) -> Result<()> {
    let mut moisture = MOISTURE.load()?.unwrap_or(65.0);
    moisture = (moisture + amount).clamp(0.0, 100.0);
    MOISTURE.save(&moisture)
}
```

`#[myrmic_sdk::evt]` is the mirror image of `publish` - and note that **the name of the function is the name of the event**: declaring `fn rain(...)` subscribes this cell to every `rain` event on the network, no matter who publishes it. There is no broker to configure and no subscription list to maintain; the handler *is* the subscription. The [Events](../../05_guides/03_events.md) guide covers the model in depth.

Now redeploy the modified cell. Your first instinct might be to run `myrmic deploy moisture-sensor` again - try it:

```text
Caused by: class 'moisture-sensor' has active instances and cannot be modified
Error: ()
```

The runtime protects a running cell: its code (its *class*) cannot be swapped underneath it. To replace a modified cell, delete the running instance first, then deploy:

```bash
myrmic delete moisture-sensor
myrmic deploy moisture-sensor
```

Glance at Terminal 3: the readings resume from 65% - not because state does not survive (it does), but because our `init` deliberately reseeds it on every deploy. A fresh pot of soil for every experiment.

Time for the first bit of magic. The CLI can publish events exactly like a cell can. Make it rain, from Terminal 2:

```bash
myrmic publish rain 20
```

Watch Terminal 3: within a second the moisture jumps up by 20 points, then the weather swing carries on from wherever that lands:

```text
[2026-08-26T16:34:02.921Z] event=moisture sender=365d6cfe-... payload=8 bytes
61.399994

[2026-08-26T16:34:03.925Z] event=moisture sender=365d6cfe-... payload=8 bytes
81.39999
```

Nothing was wired, no endpoint was called - you published an event, and the sensor's `rain` handler picked it up like any other subscriber would.

---

## What Have You Learned

- A Myrmic swarm is made of *runtimes*; `myrmic runtimes start` brings one up and `myrmic runtimes list` shows it.
- A *cell* is a small `no_std` Rust crate compiled to WebAssembly; `myrmic new` scaffolds one, `myrmic deploy` builds and deploys it, and `myrmic cells` shows what is running. Cells are addressed by a human-readable `srn` and a unique `sri`.
- Cells keep their data in `State<T>` - persistent memory that lives in the runtime's database and survives restarts.
- Cells never block: no endless loops. Periodic work is scheduled with `interval`, which invokes an ordinary command handler on the runtime's clock.
- Cells communicate through *events*: `publish` broadcasts without knowing the listeners, and an `#[evt]` handler subscribes - the name of the function is the name of the event.
- A running cell's code cannot be swapped in place: `myrmic delete`, then `myrmic deploy` again.
- The CLI is a first-class participant in the swarm: `myrmic subscribe` listens to events, and `myrmic publish` emits them, exactly like a cell would.

## Next Step

The sensor measures, but nothing in the greenhouse can *act* yet. In [Part 2 - The Pump](./02_the-pump.md) we add the first actuator - a cell that receives commands.
