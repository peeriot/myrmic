# Read values

A cell reads the world through taps. It never touches a sensor, a bus or a pin: the pipeline
does that and publishes the result under a name, and your cell looks the name up and reads it.

The SDK provides the tools needed to do this: resolving a name into a handle, reading the value
behind it, and draining events as they arrive.

This page covers reading. Everything here is the same on a microcontroller and on Linux, with
two differences called out where they matter.

## Resolve once, read often

Reading takes two steps. You resolve a name into a handle, then you read through the handle.

```rust
use myrmic_sdk::tap::Tap;

let Some(tap) = Tap::resolve("temperature")? else {
    myrmic_sdk::warn!("the pipeline publishes no `temperature` tap")?;
    return Ok(());
};

if let Some((timestamp_ms, celsius)) = tap.read_typed::<f32>()? {
    myrmic_sdk::info!("temperature is {celsius:.2} at {timestamp_ms}ms")?;
}
```

`read_typed` decodes into whatever type you name, so you get the value already in the shape you
want, along with the timestamp the pipeline stamped it with.

The two empty cases here deserve different reactions, which is why only one of them is logged.
A missing *name* means the cell and the pipeline disagree about what exists, so it is worth
saying out loud. A missing *value* is ordinary: it just means nothing has arrived yet, and a
cell that complains about it on every tick is only making noise.

The type you name has to match what the pipeline publishes. A tap declared `type: f32` reads as
`f32`. Nothing checks this for you at build time, because the cell and the pipeline are compiled
separately, so the pipeline file is the contract.

At runtime it is enforced. The typed calls compare the type you name against the one the slot
declared, before decoding anything, and a mismatch comes back as `ApiError::TypeMismatch` instead
of a value. The decode after the check is strict and refuses leftover bytes, so a type that
merely fits the front of the payload does not produce a plausible wrong number either. Only the
raw byte reads stay unchecked, by design: whoever decodes them owns the problem.

## Where to keep the handle

Resolving is a lookup, so you do not want it in the middle of a tight read loop. Keep the handle
in `InMemory`:

```rust
use myrmic_sdk::InMemory;

static TEMPERATURE: InMemory<Option<Tap>> = InMemory::empty();
```

`InMemory` is cell-local storage for exactly this: host resource handles held across handler
invocations, never written to the runtime database, gone when the cell restarts. That last part is
what you want. A handle from a previous life would mean nothing, so there is no sense in
persisting one.

Use `State` for values you want to survive a restart, and `InMemory` for handles. If you find
yourself trying to persist a `Tap`, that is the signal you have reached for the wrong one.

A kept handle can also outlive what it points at. When the pipeline behind it goes away or comes
back (on Linux, the pipeline process restarting is exactly this), reads on the old handle fail
with `ApiError::Unavailable`, and the error means precisely one thing: resolve again. It is the
one failure a cell can act on mechanically, so treat it as routine, not as a crash.

## A cell that does something

A cell can do plenty with a value: log it, aggregate it, forward it to other nodes, or act on
it. The example below acts on it, because that is where the interesting question sits, the one
the previous page was about: when should a cell decide something, and when should the pipeline?

```rust
use core::time::Duration;
use myrmic_sdk::tap::Tap;
use myrmic_sdk::{Callback, InMemory, Metadata, Result};

static TEMPERATURE: InMemory<Option<Tap>> = InMemory::empty();
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
        return Ok(());  // nothing to act on this tick; not worth logging
    };

    let hot = celsius > LIMIT_C;
    let changed = WAS_HOT.with(|was| core::mem::replace(was, hot) != hot)?;

    if changed {
        myrmic_sdk::info!("temperature crossed {LIMIT_C}: now {celsius:.2}")?;
    }

    Ok(())
}
```

Two things about this cell are worth noticing.

It only acts on a **change**, not on every reading above the limit. A cell that logs on every
tick while the value sits at 30.1 is noise.

And this decision belongs in a cell rather than in the pipeline. If it had to switch a relay as
fast as possible, or keep working while the cell is being replaced, it would belong in the
pipeline instead, as a `hysteresis` step driving an outlet. What makes it a cell's job is that
it is slower, and that a cell can do things the pipeline cannot: reach the network, talk to
other nodes, or keep history.

## The three kinds of nothing

Most of the care in a reading cell goes into telling apart three different empty answers.

**The name is not registered.** `Tap::resolve` gives `Ok(None)`. Either the pipeline does not
publish that tap, or you spelled it differently from the pipeline file.

**The tap exists but holds no value.** `read_typed` gives `Ok(None)`. This is the normal state
before the first sample arrives, and it is also what you get when the source has failed: when a
driver stops working, the pipeline clears its taps rather than leaving a stale value behind. So
an empty read means "no trustworthy value right now", which is the honest answer but not a
detailed one.

**The event queue is empty.** `take_event_typed` gives `Ok(None)`. Nothing has happened since
you last drained it.

None of these is an error, and all three are ordinary. An `Err` is different: it means the read
itself could not be served.

If you need to know *why* a value is missing, read the health tap. Every pipeline with at least
one source publishes `_signal_layer_health` as an event tap carrying which source changed state
and what it changed to, so a cell can tell "the sensor is down" from "the sensor has not
reported yet".

## Timestamps are not a clock

Every retained read hands back a timestamp in milliseconds, and it is tempting to treat it as a
time of day. It is not.

The number is milliseconds since the node started: `embassy_time::Instant` on a microcontroller,
and time since process start on Linux. It only ever moves forward, and it says nothing about
what the date is.

So you can compare two timestamps from the same node to see which reading is newer, and you can
subtract them to find out how stale a value is. What you cannot do is turn one into a wall-clock
time, or compare a timestamp from one node against a timestamp from another. They do not share
an origin.

## Events are consumed as you read them

A retained tap holds a value you can read as often as you like. An event tap is a queue, and
taking an event removes it. Drain it in a loop:

```rust
while let Some(alarm) = tap.take_event_typed::<ThresholdAlarm>()? {
    myrmic_sdk::info!("alarm at {}", alarm.value)?;
}
```

There is one sharp edge here, and it is worth reading twice.

**A wrong type does not cost you the event.** `take_event_typed` checks your type against the
slot's declared one before it consumes anything, so a mismatch is refused with the event still
queued. What does consume the event first is a payload of the *declared* type that then fails to
decode, which means the producer wrote garbage; that one is not retryable. When losing an event
would matter, `take_event` hands you the raw bytes to decode yourself, so any failure leaves you
holding the payload.

`read_typed` and `take_event_typed` decode through a 64-byte scratch buffer. On a microcontroller
a larger payload comes back as an error. On Linux it is silently truncated to the first 64 bytes;
the strict decode usually catches that, but a truncation landing exactly on a value boundary can
still decode. Either way, read into your own buffer with `read_retained` or `take_event` when you
expect a payload that size.

## When the pipeline is not there

Reads fail differently on the two platforms, and it shows up as how much a cell can tell you.

On a microcontroller the pipeline and the cell are the same firmware, so a read is a memory
lookup. It cannot really fail, and reading a name that does not exist is simply `Ok(None)`.

On Linux the pipeline is a separate process and every read is a round trip to it. If that
process is gone or unresponsive, the call is bounded rather than hanging: it gives up after five
seconds and reports failure. Two consequences follow.

A cell polling quickly against a dead pipeline will stall for that timeout on every attempt, so
its poll rate quietly collapses to the timeout. And `Tap::resolve` returning `Ok(None)` cannot
be read as "no such tap" with confidence, because an unreachable pipeline produces the same
answer. Today a cell cannot distinguish them.

A failed read is different. The connection is rebuilt automatically, but handles from before the
reconnect stop working, so a read through an old handle returns an error rather than a value.
The remedy is simple: on an error, drop the handle and resolve again. The error itself is not
very descriptive at the moment, so treat any error from a read as "re-resolve and try again"
rather than trying to interpret it.

## How often to read

There is no subscribe and no wait. A cell polls, normally from an interval timer as in the
example above.

Read at the rate you actually need values, and set the source's sample interval in the pipeline
to match. Reading faster than the pipeline samples just returns the same value again, and
sampling faster than anyone reads spends power for nothing. The two numbers are set in different
files, so it is worth checking they agree.
