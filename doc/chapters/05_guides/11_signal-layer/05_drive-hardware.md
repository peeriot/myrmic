# Drive hardware

An outlet is the write side of the Signal Layer. A cell writes a value under a name, and the
driver bound to that name turns it into something physical: a relay closing, a fan speeding up.

The cell never touches the pin. It writes a value to the outlet and the pipeline carries it the rest of the
way, which is what makes the same cell work on a board wired differently.

The SDK provides the tools needed to do this: resolving an outlet by name and writing a typed
value into it.

## Resolve and write

Writing mirrors reading. Resolve the name once, then write through the handle.

```rust
use myrmic_sdk::outlet::Outlet;
use myrmic_sdk::signal_layer::DigitalState;

let Some(heater) = Outlet::resolve("heat_relay")? else {
    myrmic_sdk::warn!("the pipeline publishes no `heat_relay` outlet")?;
    return Ok(());
};

heater.write_typed(&DigitalState { on: true })?;
```

The type has to match what the pipeline declared for that outlet. A `DigitalState` for an on/off
device, a `PwmDuty` for a duty fraction.

Get this wrong and you are told, twice. `write_typed` compares your type against the outlet's
declared one before anything crosses the boundary, and a mismatch fails with
`ApiError::TypeMismatch`. Behind that, the pipeline strictly decodes every arriving payload and
refuses one that does not decode exactly into the declared type, so writing a duty fraction to an
on/off outlet is an error, not a silently switched relay. The raw `write` skips only the
cell-side check; the pipeline's strict decode still stands behind it.

As with taps, keep the handle in `InMemory` rather than resolving on every write, and note the
encoding buffer is 64 bytes, which every built-in outlet type fits comfortably.

## What a successful write means

This is the part worth being precise about.

`write_typed` returning `Ok(())` means **the value was stored**. It does not mean the relay has
switched. The value lands in the outlet's slot with a timestamp, and a separate task belonging
to that device picks it up and applies it on its next tick.

That tick is the outlet's `write_interval_ms`, which defaults to 100 ms and is set in the
pipeline. So there is a delay of up to one tick between a successful write and anything moving,
and the write itself tells you nothing about whether the hardware accepted it.

The pipeline re-applies whenever a *new* write arrives, not when the value changes, so writing
the same value twice does reach the driver twice. What happens next is up to the driver.

The built-in output drivers can rate-limit themselves, refusing to act again within a minimum
interval, and the digital one ignores a write asking for the state it is already in. But the interval
defaults to **zero, which means off**: unless the board file sets it, nothing is throttled. If
your hardware has a switching limit, set it deliberately rather than assuming it is there.

## The example, continued

On the [read values](./03_read-values.md) page there is a cell that watches a temperature and
logs when it crosses a limit. Here is the same cell switching a heater instead of logging.

```rust
use core::time::Duration;
use myrmic_sdk::outlet::Outlet;
use myrmic_sdk::signal_layer::DigitalState;
use myrmic_sdk::tap::Tap;
use myrmic_sdk::{Callback, InMemory, Metadata, Result};

static TEMPERATURE: InMemory<Option<Tap>> = InMemory::empty();
static HEATER: InMemory<Option<Outlet>> = InMemory::empty();
static WAS_HOT: InMemory<bool> = InMemory::new(false);

const LIMIT_C: f32 = 30.0;

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    myrmic_sdk::interval(Callback::of::<check>(), Duration::from_secs(2))
        .build()
        .map_err(|_| "failed to create timer")?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn check(_md: Metadata) -> Result<()> {
    let reading = TEMPERATURE.with(|slot| {
        if slot.is_none() {
            *slot = Tap::resolve("temperature").ok().flatten();
        }
        slot.as_ref().and_then(|tap| tap.read_typed::<f32>().ok().flatten())
    })?;

    let Some((_timestamp_ms, celsius)) = reading else {
        return Ok(());  // nothing to act on this tick
    };

    let hot = celsius > LIMIT_C;
    let changed = WAS_HOT.with(|was| core::mem::replace(was, hot) != hot)?;
    if !changed {
        return Ok(());  // already in the right state; do not rewrite
    }

    HEATER.with(|slot| {
        if slot.is_none() {
            *slot = Outlet::resolve("heat_relay").ok().flatten();
        }
        if let Some(outlet) = slot.as_ref() {
            let _ = outlet.write_typed(&DigitalState { on: !hot });
        }
    })?;

    Ok(())
}
```

The cell writes only on a change, which matters more here than it did when it was logging. A
relay switched every two seconds because a value hovers around the limit is a relay wearing out.

Notice there is still no hysteresis. The cell flips the moment the reading crosses 30, so a
value dithering across the limit still chatters. Fixing that in the cell means tracking two
thresholds; the pipeline already has a step that does it.

## When the write does not take effect

A write that was stored can still fail at the hardware.

If applying the value fails, the pipeline logs it and publishes a fault on the outlet's error
tap, which you can read like any other event tap:

```rust
let faults = Tap::resolve("relay_fault")?;   // declared in the pipeline, source: heat_relay.error
```

The fault says which half broke. `WriteFailed` means the value could not be applied to the
device. `ReadFailed` means the driver could not read the device's state back, which only applies
to devices wired to report it.

A failed apply is retried. The pipeline does not mark the value as applied unless it succeeds,
so the next tick tries the same value again, and it keeps trying until it works or a new
write replaces it. A cell does not need to implement its own retry.

One case is not a failure and so is not retried. When a driver declines a write because it
arrived inside its minimum interval, it reports success, and the pipeline records the value as
applied. That write is dropped: no fault, no retry, and the hardware never moved. It is the one
way a write can be silently lost, and it only happens once the interval is configured.

## Knowing what actually happened

A write tells you a value was stored. A fault tells you an attempt failed. Neither tells you
the device is in the state you asked for.

For that the hardware has to be wired to report back. A device with a feedback input gets its
real state published as a tap, read from a separate line rather than inferred from the write:

```yaml
taps:
  - name: relay_contact
    kind: retained
    type: bool
    source: relay_cmd.contact       # a second outlet, on a feedback-capable device
```

That tap is a genuine read of the device, so it will disagree with the written value when something is
wrong, which is the entire point. A successful write never populates it, and a fault never
populates it either: status comes only from a real read-back.

If knowing the true state matters, wire the feedback and use a driver that reads it. If it does
not, do not pretend the write is confirmation.

## One writer per device

A device can be driven by exactly one outlet. Declare two outlets on the same device and
generation fails with `device 'x' is already driven by another outlet (single-writer per
device)`.

This is deliberate. Two writers with no ordering between them would leave the device in whichever
state happened to be applied last, and nothing would be wrong enough to report. If two cells need
to influence one actuator, have them agree in the layer above and write once.

## Should this be in the pipeline instead?

The cell above is a reasonable use of an outlet: it decides something slowly, at a rate a human
would care about, and it does not matter if it pauses briefly while the cell is replaced.

Change any of those and the answer changes. If the relay has to react as fast as the sensor can
report, or has to keep working while a cell is reloaded, the decision belongs in the pipeline as
a step driving the outlet directly, with no cell in the path. The
[pipeline page](./02_design-your-pipeline.md) covers how to wire that, and the trade is the same
one it describes: the pipeline for reflexes, the cell for judgement.
