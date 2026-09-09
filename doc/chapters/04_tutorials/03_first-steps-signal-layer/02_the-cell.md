# Part 2 - The Cell

This is Part 2 of [First Steps with the Signal Layer](../03_first-steps-signal-layer.md). The
pipeline from Part 1 is running and serving two taps. Now you start a runtime, write a cell that
reads them, and watch the values flow.

---

## Step 1 - Start a runtime

A runtime hosts cells. In a **new terminal** (the pipeline stays running in its own), start one —
it runs in the foreground, so leave it up:

```bash
myrmic runtimes start
```

From a third terminal, confirm it:

```bash
myrmic runtimes list
```

```text
default	running	pid=26094
```

The runtime is named `default`. It announces itself on the local network, and everything that
joins the swarm later — including the ESP32 in Part 3 — finds it by that announcement. Nothing
about its address needs writing down.

## Step 2 - Scaffold the cell

```bash
myrmic new ~/sl-tutorial/thermometer --sdk ~/myrmic
```

The scaffold is a small counter example. Replace `thermometer/src/lib.rs` entirely:

```rust
//! Tutorial cell: reads the Signal Layer taps once a second and logs them.
#![no_std]

use core::time::Duration;

use myrmic_sdk::tap::Tap;
use myrmic_sdk::{Callback, Metadata, Result};

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

/// Timer target: read both taps and log what the pipeline is publishing.
#[myrmic_sdk::cmd]
fn sample(_md: Metadata) -> Result<()> {
    let Some(value_tap) = Tap::resolve("sim_value")? else {
        myrmic_sdk::info!("sim_value not offered here (yet)").ok();
        return Ok(());
    };
    let Some(avg_tap) = Tap::resolve("sim_avg")? else {
        myrmic_sdk::info!("sim_avg not offered here (yet)").ok();
        return Ok(());
    };

    match (value_tap.read_typed::<f32>()?, avg_tap.read_typed::<f32>()?) {
        (Some((ts, value)), Some((_, avg))) => {
            myrmic_sdk::info!("t={} value={} avg={}", ts, value, avg).ok();
        }
        _ => {
            myrmic_sdk::info!("taps exist but hold no value yet").ok();
        }
    }
    Ok(())
}
```

The whole cell is: a timer firing once a second, two tap resolves, two typed reads, one log line.
`resolve` answers with nothing when a tap is not offered on this node, and `read_typed` answers
with nothing when the tap exists but holds no value yet; the
[reading guide](../../05_guides/11_signal-layer/03_read-values.md) explains the different kinds of
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
  cell         sri           kind  runtime     age  class        srn
  thermometer  e6f23498-...  wasm  [c]91d1706  3s   thermometer  thermometer
```

The `runtime` column shows the first characters of the runtime's id; right now that is the runtime
on this machine. Remember the shape of this row — in Part 3 that column is the proof that the cell
moved.

## Step 4 - Watch it read

Cell logs land in the runtime's log file:

```bash
tail -f ~/.local/share/myrmic/*/logs/runtime.$(date +%F).log
```

Expected output, one line per second:

```text
INFO wasm_log: ... t=18500 value=40 avg=25 ...
INFO wasm_log: ... t=19500 value=60 avg=45 ...
INFO wasm_log: ... t=20500 value=80 avg=65 ...
INFO wasm_log: ... t=21500 value=100 avg=85 ...
INFO wasm_log: ... t=22500 value=10 avg=50 ...
```

The value climbs (that is the simulated sensor) and the average trails it by the four-sample
window (that is the `moving-average` step doing its job). The simulated value is a sawtooth: it
wraps from 100 back to 0, and for the next few seconds the average takes the plunge in steps. That
dip is the moving average being correct, not broken: `(80 + 100 + 10 + ...) / 4` is simply lower.
Your cell is reading live values out of the Signal Layer through the SDK, over the socket from
Part 1.

One thing to know before you restart anything: a deployed cell does not survive its runtime being
deleted. If you stop the runtime and start fresh, deploy the cell again.
