# Part 2 - The Cell

This is Part 2 of [First Steps with the Signal Layer](../03_first-steps-signal-layer.md). The
pipeline from Part 1 is running and serving two taps. Now you start a runtime, write a cell that
reads them, and watch the values flow.

---

## Step 1 - Start a runtime

A runtime hosts cells. In a **new terminal** (the pipeline stays running in its own), start one and
leave it up:

```bash
myrmic runtimes start
```

Starting a runtime and confirming it with `myrmic runtimes list` are covered in
[the quickstart](../../01_quickstart.md). The part that matters here: the runtime is named
`default` and announces itself on the local network, so everything that joins the swarm later —
including the ESP32 in Part 3 — finds it by that announcement, with no address to write down.

## Step 2 - Scaffold the cell

```bash
myrmic new ~/sl-tutorial/thermometer
```

This builds against the same `PEERIOT_MYRMIC_SDK` checkout from the
[prerequisites](../03_first-steps-signal-layer.md) (or pass `--sdk <path>`). The scaffold is a small
counter example. Replace `thermometer/src/lib.rs` entirely:

```rust
//! Tutorial cell: reads the Signal Layer taps once a second and logs them.
#![no_std]

use core::time::Duration;

use myrmic_sdk::tap::Tap;
use myrmic_sdk::{ApiError, Callback, InMemory, Metadata, Result};

// Resolve each tap once and reuse the handle. `InMemory` is cell-local state
// that outlives a single handler call but is never persisted.
static VALUE_TAP: InMemory<Option<Tap>> = InMemory::empty();
static AVG_TAP: InMemory<Option<Tap>> = InMemory::empty();

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    myrmic_sdk::info!("thermometer starting").ok();
    // The handle is only needed to cancel the timer; the timer itself keeps
    // running without it.
    let _timer = myrmic_sdk::interval(Callback::of::<sample>(), Duration::from_secs(1))
        .fixed_delay()
        .build()
        .map_err(|_| "timer failed")?;
    Ok(())
}

/// Read a tap through its cached handle: resolve it the first time, reuse it
/// after, and drop it if the pipeline reconnected so the next tick re-resolves.
fn read(cache: &InMemory<Option<Tap>>, name: &str) -> Result<Option<(u64, f32)>> {
    let mut slot = cache.try_borrow_mut()?;
    if slot.is_none() {
        *slot = Tap::resolve(name)?;
    }
    let result = match slot.as_ref() {
        Some(tap) => tap.read_typed::<f32>(),
        None => return Ok(None), // not offered on this node
    };
    match result {
        Ok(reading) => Ok(reading),
        Err(ApiError::Unavailable) => {
            *slot = None; // stale handle after a reconnect; re-resolve next tick
            Ok(None)
        }
        Err(err) => Err(err.into()),
    }
}

/// Timer target: read both taps and log what the pipeline is publishing.
#[myrmic_sdk::cmd]
fn sample(_md: Metadata) -> Result<()> {
    match (read(&VALUE_TAP, "sim_value")?, read(&AVG_TAP, "sim_avg")?) {
        (Some((ts, value)), Some((_, avg))) => {
            myrmic_sdk::info!("t={} value={} avg={}", ts, value, avg).ok();
        }
        _ => {
            myrmic_sdk::info!("taps not ready yet").ok();
        }
    }
    Ok(())
}
```

The cell resolves each tap once and reuses the handle. Since `Tap::resolve` is a host call, the
`read` helper caches the handle in an `InMemory` slot (cell-local state that lives as long as the
cell instance but is never persisted) and only resolves again when the slot is empty. `read`
answers with nothing in three cases: the tap is not offered on this node (`resolve` found nothing),
it holds no value yet (`read_typed` found nothing), or a pipeline reconnect left the handle stale
(`ApiError::Unavailable`), in which case it drops the handle so the next tick resolves a fresh one.
The [reading guide](../../05_guides/11_signal-layer/03_read-values.md) explains the kinds of
nothing. The scaffold pins `myrmic-sdk` to your checkout, so the cell and the runtime cannot drift
apart.

## Step 3 - Deploy

`deploy` builds the cell and starts it on the swarm in one step (it defaults to the `linux`
platform):

```bash
cd ~/sl-tutorial/thermometer
myrmic deploy . --name thermometer
```

```text
INFO  deploying cell (srn = thermometer, sri = e6f23498-14fb-50d0-8a2a-2baff1b07f22)
INFO  deployed cell (srn = thermometer, sri = e6f23498-14fb-50d0-8a2a-2baff1b07f22)
```

Then look at where the cell landed:

```bash
myrmic cells status
```

```text
  cell         sri           kind  runtime     age  policy  class        srn
  thermometer  e6f23498-...  wasm  [c]91d1706  3s   never   thermometer  thermometer
```

The `runtime` column shows the first characters of the runtime's id; right now that is the runtime
on this machine. Remember the shape of this row — in Part 3 that column is the proof that the cell
moved.

## Step 4 - Watch it read

The runtime records what every cell logs in its built-in telemetry database, and the `myrmic
telemetry` CLI reads it back, with no log files and no Grafana. The database keeps entries only
within a retention window, and none is set by default, so turn one on first:

```bash
myrmic telemetry set-db-retention 1h
```

Then read the logs:

```bash
myrmic telemetry logs
```

It prints the stored entries oldest-first, colour-coded by severity, each with a timestamp, its
trace id, and the line the cell logged. It is a snapshot, not a live stream: run it again to see the
lines added since. Retention takes effect from the moment you set it, so what you see is what the
cell has logged since, one reading per second:

```text
INFO  [2026-09-10T14:07:18.500+02:00] | trace_id = 25975cd9... | t=18500 value=40 avg=25
INFO  [2026-09-10T14:07:19.500+02:00] | trace_id = 3bcc1f04... | t=19500 value=60 avg=45
INFO  [2026-09-10T14:07:20.500+02:00] | trace_id = e566233f... | t=20500 value=80 avg=65
INFO  [2026-09-10T14:07:21.500+02:00] | trace_id = d74e864c... | t=21500 value=100 avg=85
INFO  [2026-09-10T14:07:22.500+02:00] | trace_id = e0560768... | t=22500 value=10 avg=50
```

The value climbs (that is the simulated sensor) and the average trails it by the four-sample
window (that is the `moving-average` step doing its job). The simulated value is a sawtooth: it
wraps from 100 back to 0, and for the next few seconds the average takes the plunge in steps. That
dip is the moving average being correct, not broken: `(80 + 100 + 10 + ...) / 4` is simply lower.
Your cell is reading live values out of the Signal Layer through the SDK, over the socket from
Part 1.

One thing to know before you restart anything: a deployed cell does not survive its runtime being
deleted. If you stop the runtime and start fresh, deploy the cell again.
