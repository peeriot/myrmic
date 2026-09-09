# Part 3 - One Sensor per Bed

This is Part 3 of the [Scaling Your Application](../04_scaling.md) tutorial. You build a **sensor root** that spawns one mock sensor per probe, and you pair each bed with its own sensor - **learning how spawned cells find each other by name alone, and how a cell filters a broadcast event down to its partner.**

It continues where [Part 2](./02_bringing-beds-back.md) left off: four runtimes, the root and four beds running, copies on every node.

---

## Step 10 - The Sensor Root

Remember the end of Part 1: you published one `moisture` reading and every bed took it. Events are delivered by name, to every cell that handles that name, so with real sensors every bed would hear every probe in the greenhouse. A bed needs its own sensor, and it needs to ignore the others.

First the sensors. The mock sensor from the Smart Greenhouse does exactly what a probe should - publish `moisture` once a second - and nothing in it changes. Copy it over, and scaffold the root that will spawn it:

```bash
cp -r ../greenhouse/moisture-sensor .
myrmic new sensor-root
```

The sensor root is the bed root of Part 2 with two words changed: it spawns the `moisture-sensor` class instead of `grow-bed`, and it calls its children `sensor-<n>`. Everything else - the list, `ensure`, the retry, the monitor handler - is the same pattern doing the same job. Replace the content of `sensor-root/src/lib.rs` with:

```rust
//! Sensor root: creates one moisture-sensor child per probe and brings lost sensors back.
#![no_std]

use core::time::Duration;

use myrmic_sdk::db::state::State;
use myrmic_sdk::monitor::{CellLost, LostReason};
use myrmic_sdk::{Callback, ClassHandle, Metadata, Result, SpawnError, String, Vec, format};

/// The class to spawn sensors from, resolved when the application is deployed.
const SENSOR: ClassHandle = myrmic_sdk::declare!("moisture-sensor");

/// One sensor this root has created: its index and the sector it runs in.
#[derive(serde::Serialize, serde::Deserialize)]
struct Sensor {
    index: u32,
    sector: String,
}

/// The sensors this root has created.
const SENSORS: State<Vec<Sensor>> = State::new_const("sensors");

/// Spawns `sensor-<index>` on a node of `sector`.
fn spawn_sensor(index: u32, sector: &str) -> core::result::Result<(), SpawnError> {
    SENSOR
        .new()
        .name(format!("sensor-{index}"))
        .tag(sector)
        .spawn()
        .map(|_| ())
}

/// How long to wait before trying again when a sensor could not be spawned.
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

/// Timer target: makes sure every sensor in the list is running. A sensor that
/// already runs is fine; a sensor that cannot be spawned right now is tried
/// again later.
#[myrmic_sdk::cmd]
fn ensure(_md: Metadata) -> Result<()> {
    myrmic_sdk::info!("ensure: running")?;
    let mut missing = false;

    for sensor in SENSORS.load()?.unwrap_or_default() {
        match spawn_sensor(sensor.index, &sensor.sector) {
            Ok(()) => myrmic_sdk::info!("sensor-{} spawned", sensor.index)?,
            Err(SpawnError::AlreadyExists) => {
                myrmic_sdk::info!("sensor-{} already running", sensor.index)?
            }
            Err(err) => {
                myrmic_sdk::warn!("sensor-{} not running: {}", sensor.index, <&str>::from(err))?;
                missing = true;
            }
        }
    }

    if missing {
        retry_later()?;
    }

    Ok(())
}

/// Creates the next sensor in a sector: `myrmic send sensor-root add_sensor '"sectorA"'`.
#[myrmic_sdk::cmd]
fn add_sensor(_md: Metadata, sector: String) -> Result<()> {
    let mut sensors = SENSORS.load()?.unwrap_or_default();
    let index = sensors.last().map_or(1, |last| last.index + 1);

    // A failed handler is delivered again; a refused spawn should not be retried.
    if let Err(err) = spawn_sensor(index, &sector) {
        myrmic_sdk::warn!(
            "sensor-{index} in '{sector}' refused: {}",
            <&str>::from(err)
        )?;
        return Ok(());
    }

    sensors.push(Sensor { index, sector });
    SENSORS.save(&sensors)?;
    myrmic_sdk::info!("added sensor-{index}")?;

    Ok(())
}

/// The swarm tells a parent when one of its children is gone.
#[myrmic_sdk::monitor]
fn child_lost(_md: Metadata, loss: CellLost) -> Result<()> {
    // Only losses worth repairing: the node died, or the sensor crashed.
    if !matches!(loss.reason, LostReason::NodeLost | LostReason::Crashed) {
        return Ok(());
    }

    let sensors = SENSORS.load()?.unwrap_or_default();
    let name = loss.local_name.as_deref().unwrap_or("");
    let Some(sensor) = sensors
        .iter()
        .find(|s| format!("sensor-{}", s.index) == name)
    else {
        return Ok(());
    };

    myrmic_sdk::warn!("{name} lost; respawning in '{}'", sensor.sector)?;
    if let Err(err) = spawn_sensor(sensor.index, &sensor.sector) {
        myrmic_sdk::warn!("{name} could not be respawned: {}", <&str>::from(err))?;
        retry_later()?;
    }

    Ok(())
}
```

One difference is worth a look: `spawn_sensor` passes no `.payload(...)`. The sensor's `init` declares no argument, and a cell that declares none must receive none, so the root hands it nothing. The sensor does not know which bed it feeds, and it does not need to. The pairing is the bed's business.

---

## Step 11 - The Bed Listens to One Sensor

The pairing rule is the naming: `sensor-<n>` feeds `bed-<n>`. A child's identity is derived from its parent's name and its own, so `bed-3` can compute the identity of `sensor-root/sensor-3` from the index it was spawned with - nobody has to tell it. Three changes to `grow-bed/src/lib.rs`. Import `Sri` and `format`:

```rust
use myrmic_sdk::{Metadata, Result, Sri, format, publish};
```

Add a second piece of state, next to `BED`:

```rust
/// The one sensor this bed listens to.
const SENSOR: State<Sri> = State::new_const("sensor");
```

Compute and store it in `init`, before the bed's own state:

```rust
    let sensor = Sri::of_path(&format!("sensor-root/sensor-{index}")).map_err(|_| "bad name")?;
    SENSOR.save(&sensor)?;
```

And make the `moisture` handler check who is talking:

```rust
#[myrmic_sdk::evt]
fn moisture(md: Metadata, value: f32) -> Result<()> {
    if SENSOR.load()? != Some(md.sender) {
        return Ok(());
    }
    ...
```

`md.sender` is the identity of the cell that published the event, stamped by the runtime - a sender cannot forge it. Every bed still receives every `moisture` event, because that is what a handler for `moisture` means; the bed returns early for all but its partner's. The complete file is at the end of this part.

Why filter instead of listening to a per-bed event such as `moisture_3`? Because a cell's event handlers are fixed when it is compiled: `#[evt]` exports one function per event name, and the runtime discovers those exports when the cell is deployed. There is no runtime subscribe. The handler is where the choice is made. At tutorial scale the cost is one state read per bed per reading; at a thousand sensors it would be time to let the sensor *address* its bed instead, with a command to the bed's SRI, computed by the same naming rule.

---

## Step 12 - Deploy and Pair

Two classes and one instance join `app_specs.yml`:

```yaml
name: greenhouse

classes:
  - id: bed-root
    build: ./bed-root
  - id: sensor-root
    build: ./sensor-root
  - id: grow-bed
    build: ./grow-bed
  - id: moisture-sensor
    build: ./moisture-sensor

instances:
  - class: bed-root
    restart: always
  - class: sensor-root
    restart: always
```

Remove and deploy, as before:

```bash
myrmic delete greenhouse --app
myrmic deploy app_specs.yml
myrmic cells
```

```text
  cell         sri                                   kind  runtime     age  policy  class        srn
──── greenhouse ────────────────────────────────────────────────────────────────────────────────────────────
  bed-root     ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  9s   always  bed-root     bed-root
  ├─ bed-1     777e343f-cc44-5964-850d-9302dbd591ee  wasm  [c]a157d28  8s   —       grow-bed     bed-root/bed-1
  ├─ bed-2     f1356c6d-8603-5fa5-a659-35764d15a3bf  wasm  [d]40df21c  8s   —       grow-bed     bed-root/bed-2
  ├─ bed-3     72fdd5d5-ca6b-54af-b017-c624f8b8b1d7  wasm  [1]12a7f3f  7s   —       grow-bed     bed-root/bed-3
  └─ bed-4     22bffd66-a421-5ddb-8f3e-6530c8fd010a  wasm  [4]f741c9b  7s   —       grow-bed     bed-root/bed-4
  sensor-root  904b6b3b-8411-5ff5-8f0d-56555e49b75e  wasm  [4]f741c9b  9s   always  sensor-root  sensor-root
```

The four beds are back on their own, as in Part 2, now with the new grow-bed code; the sensor root is up with nothing under it. Give each bed its sensor, in the bed's sector:

```bash
myrmic send sensor-root add_sensor '"sectorA"'
myrmic send sensor-root add_sensor '"sectorB"'
myrmic send sensor-root add_sensor '"sectorA"'
myrmic send sensor-root add_sensor '"sectorB"'
myrmic cells
```

```text
  cell         sri                                   kind  runtime     age  policy  class            srn
──── greenhouse ────────────────────────────────────────────────────────────────────────────────────────────────────────
  bed-root     ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  15s  always  bed-root         bed-root
  ├─ bed-1     777e343f-cc44-5964-850d-9302dbd591ee  wasm  [c]a157d28  14s  —       grow-bed         bed-root/bed-1
  ├─ bed-2     f1356c6d-8603-5fa5-a659-35764d15a3bf  wasm  [d]40df21c  14s  —       grow-bed         bed-root/bed-2
  ├─ bed-3     72fdd5d5-ca6b-54af-b017-c624f8b8b1d7  wasm  [1]12a7f3f  14s  —       grow-bed         bed-root/bed-3
  └─ bed-4     22bffd66-a421-5ddb-8f3e-6530c8fd010a  wasm  [4]f741c9b  14s  —       grow-bed         bed-root/bed-4
  sensor-root  904b6b3b-8411-5ff5-8f0d-56555e49b75e  wasm  [4]f741c9b  15s  always  sensor-root      sensor-root
  ├─ sensor-1  152b9c54-519c-55e4-b828-ecc811f5325c  wasm  [c]a157d28  6s   —       moisture-sensor  sensor-root/sensor-1
  ├─ sensor-2  880e731a-aa75-5bab-9377-73b271c5643a  wasm  [d]40df21c  6s   —       moisture-sensor  sensor-root/sensor-2
  ├─ sensor-3  025bda92-5332-557a-a2a7-ef8d8933c4a5  wasm  [1]12a7f3f  6s   —       moisture-sensor  sensor-root/sensor-3
  └─ sensor-4  b86e8291-adb5-55e3-b058-1352fc57b966  wasm  [4]f741c9b  6s   —       moisture-sensor  sensor-root/sensor-4
```

Two trees now. The sensors sit in the sectors you asked for; whether a sensor shares a node with its bed is the swarm's choice and does not matter - the pairing is by name, not by place. Look at the beds in Terminal 2:

```bash
myrmic subscribe bed_state
```

```text
[2026-09-08T18:56:12.734Z] event=bed_state sender=777e343f-cc44-5964-850d-9302dbd591ee payload=32 bytes
{
  "index": 1,
  "moisture": 54.599945
}

[2026-09-08T18:56:12.735Z] event=bed_state sender=f1356c6d-8603-5fa5-a659-35764d15a3bf payload=32 bytes
{
  "index": 2,
  "moisture": 54.599945
}

[2026-09-08T18:56:12.829Z] event=bed_state sender=72fdd5d5-ca6b-54af-b017-c624f8b8b1d7 payload=32 bytes
{
  "index": 3,
  "moisture": 54.599945
}

[2026-09-08T18:56:13.677Z] event=bed_state sender=22bffd66-a421-5ddb-8f3e-6530c8fd010a payload=32 bytes
{
  "index": 4,
  "moisture": 55.799946
}
```

Each bed reports once a second, when *its* sensor reads. The values are close because the four mock sensors started within the same second and follow the same simulated weather; what tells them apart is the timing, and the `sender`. Now do what you did at the end of Part 1 - play a sensor from the CLI:

```bash
myrmic publish moisture 1.0
```

Nothing. Terminal 2 keeps showing the sensors' readings, and no bed ever reports `1.0`. Every bed received the event - the CLI publishes like any other sender - and every bed dropped it, because the sender was not its partner. In Part 1 the same command moved all the beds at once.

---

## The Complete Code

The grow-bed, as it stands after this part:

```rust
//! Grow-bed asset: the canonical state of one bed of plants, fed by one sensor.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, Sri, format, publish};

/// What the bed knows - also the payload of the `bed_state` event.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct Bed {
    index: u32,
    moisture: f32,
}

const BED: State<Bed> = State::new_const("bed");

/// The one sensor this bed listens to.
const SENSOR: State<Sri> = State::new_const("sensor");

/// The bed root passes the bed's index when it spawns us. `sensor-<index>`
/// under `sensor-root` is our sensor, and its identity follows from the name.
#[myrmic_sdk::init]
fn init(_md: Metadata, index: u32) -> Result<()> {
    let sensor = Sri::of_path(&format!("sensor-root/sensor-{index}")).map_err(|_| "bad name")?;
    SENSOR.save(&sensor)?;

    let bed = BED.load()?.unwrap_or(Bed {
        index,
        moisture: 0.0,
    });
    BED.save(&bed)?;

    publish("bed_state", &bed)
}

/// A new reading from any sensor: keep it only if it comes from ours.
#[myrmic_sdk::evt]
fn moisture(md: Metadata, value: f32) -> Result<()> {
    if SENSOR.load()? != Some(md.sender) {
        return Ok(());
    }

    let mut bed = BED.load()?.ok_or("bed not initialised")?;
    bed.moisture = value;
    BED.save(&bed)?;

    publish("bed_state", &bed)
}
```

The sensor, unchanged from the Smart Greenhouse:

```rust
//! Mock soil-moisture sensor: publishes a `moisture` reading every second.
//! The simulated moisture swings between a dry and a wet bound, the way real
//! soil follows the weather.
//!
//! Part 1 of the Smart Greenhouse tutorial.
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

/// Timer target: the simulation advances one step, then the reading is
/// published. The value swings: it dries down to 50%, turns, wets up to 80%.
#[myrmic_sdk::cmd]
fn measure(_md: Metadata) -> Result<()> {
    let mut moisture = MOISTURE.load()?.unwrap_or(65.0);
    let mut rising = RISING.load()?.unwrap_or_default();

    if moisture <= 50.0 {
        rising = true;
    } else if moisture >= 80.0 {
        rising = false;
    }

    let delta = if rising { 1.2 } else { -0.4 };
    moisture = (moisture + delta).clamp(0.0, 100.0);

    MOISTURE.save(&moisture)?;
    RISING.save(&rising)?;

    publish("moisture", &moisture)
}

/// A rain shower adds a one-time amount of moisture.
#[myrmic_sdk::evt]
fn rain(_md: Metadata, amount: f32) -> Result<()> {
    let mut moisture = MOISTURE.load()?.unwrap_or(65.0);
    moisture = (moisture + amount).clamp(0.0, 100.0);

    MOISTURE.save(&moisture)
}
```

---

## What Have You Learned

- **The same root pattern, twice.** A list, `spawn_<child>`, `add_<child>`, `ensure` with a retry, a monitor handler. The sensor root is the bed root with the class and the names changed.
- **A child receives only what its `init` declares.** The sensor takes no payload, so the root passes none.
- **Pairing by name.** `bed-3` derives the identity of `sensor-root/sensor-3` from its own index, with `Sri::of_path`. No configuration, no lookup, no message.
- **Events are delivered by name; the handler chooses.** Handlers are fixed at compile time, so a cell narrows a broadcast by checking `md.sender`, which the runtime stamps. At large scale, an addressed command is the alternative.
- **Redeploying the application rebuilt the beds by itself.** The roots' lists are in their state, and the state outlives the application.

Next: [Part 4 - The Dashboard](./04_the-dashboard.md), where the greenhouse grows from a web page.
