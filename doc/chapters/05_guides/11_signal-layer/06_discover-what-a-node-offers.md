# Discover what a node offers

Most cells are written against a pipeline you already know. You know there is a tap called
`temperature` because you wrote the pipeline file, and the cell simply resolves that name.

Some cells cannot work that way. A cell that logs whatever a node produces, or reports what a
device is capable of, or gets deployed across boards that are wired differently, has to ask the
node what it has rather than assume. That is what the registry is for.

The SDK provides the tools needed to ask: counting what a node publishes, and walking the list to
get each name and the kind of slot behind it.

## Walking the registry

The registry gives you a count and an entry per index:

```rust
use myrmic_sdk::tap::{self, Tap, TapKind};

const MAX_NAME: usize = 64;

let count = tap::list_len()?;

for i in 0..count {
    let mut name_buf = [0u8; MAX_NAME];
    let Some((name_len, kind)) = tap::list_entry(i, &mut name_buf)? else {
        continue;
    };
    let name = core::str::from_utf8(&name_buf[..name_len]).unwrap_or("?");

    myrmic_sdk::info!("{name} is a {kind:?} tap")?;
}
```

Names come back as bytes into a buffer you provide, so pick a size and stick to it. Entries are
indexed, and an index that no longer exists gives `Ok(None)` rather than an error, so a loop that
races a changing registry skips rather than fails.

Having a name, you resolve it exactly as you would a name you knew in advance.

## What you learn, and what you do not

This is the part that shapes every discovering cell.

The registry tells you **two** things about a tap: its name, and its kind. Kind is the shape of
the slot, not the shape of the value: `Retained` for a slot holding the latest value, `Event` for
a queue you drain.

It does **not** tell you the value's type. There is no way to ask whether `temperature` is an
`f32` or something else. The pipeline file knows, the generated code knows, and the cell does
not.

So a discovering cell has to decide what to do about a value whose type it cannot know. There
are three honest answers: know the type anyway because the name is a convention you control;
try a likely type and cope when you are wrong; or read the raw bytes and make no assumption.

Which of those is safe depends entirely on the kind, and the difference is sharp.

## Probing a retained tap is free; probing an event tap is not

A retained tap holds its value. Reading it does not consume it, so a failed decode costs you
nothing: you tried `f32`, it was not an `f32`, the value is still there and you can move on.
Guessing is a perfectly reasonable strategy.

```rust
match tap.read_typed::<f32>() {
    Ok(Some((ts, value))) => myrmic_sdk::info!("{name} = {value:.3} at {ts}ms")?,
    Ok(None) => {}          // no value yet
    Err(_) => {}            // did not decode as f32
}
```

A wrong guess fails honestly. The typed read checks your type against the one the tap declared
before decoding, so asking for an `f32` from a tap that holds something else returns an error
rather than a plausible wrong number. Probing a retained tap is therefore safe for the tap *and*
for your conclusions.

An event tap is a queue, and a successful take removes the event. A wrong-typed guess is refused
before anything is consumed, so guessing no longer destroys data; what a *right* guess consumes is
a real event, taken away from whatever cell this node runs. Probing an event tap is safe when you
are wrong and intrusive when you are right, which is a strange enough property to remember.

When you would rather look without taking, there is nothing for it: taking is the only read an
event tap has. Either know the type from the name, or take the raw bytes with `take_event` and
decode them yourself.

The one event tap whose type is always known is the health tap: `_signal_layer_health` carries a
`HealthEvent`, on every pipeline that has sources.

## Outlets enumerate the same way

Outlets have the same two functions, with the same shapes:

```rust
use myrmic_sdk::outlet::{self, Outlet};

let count = outlet::list_len()?;
let Some((name_len, _kind)) = outlet::list_entry(i, &mut name_buf)? else { continue };
```

Two differences are worth knowing. Outlet entries report their kind using the same type as taps,
and today it is always `Retained`, because an outlet is a last-value-wins write slot. The
variant exists so a future event-shaped outlet can be added without breaking the interface, so do
not read meaning into it yet.

And discovering an outlet tells you even less than discovering a tap. You learn that a node can be
driven under that name, but not what value type it accepts. A wrong-typed guess is refused, the
same check as on taps; but a write of the *declared* type is accepted and acted on, whatever your
reason for sending it was. Guessing at
an outlet can move hardware. Enumerating outlets is for reporting what a node can do, not for
deciding to do it.

## Sizes and limits

A registry is small and bounded. Ask for the count rather than assuming, but do not expect to
find hundreds of entries: a pipeline publishes what a cell needs, not everything it knows.

The health tap counts toward the same budget as the taps you declared, so a pipeline with sources
always has at least one entry even if it declares none of its own.

Give the name buffer a fixed size that comfortably fits your naming convention. A name longer
than the buffer is truncated rather than reported, so a generous buffer costs you a few bytes of
stack and saves a confusing bug.

## When not to enumerate

If your cell is written for a pipeline you control, resolve the names directly. Enumerating to
find a name you already know adds a loop, a buffer, and a class of bug where a rename silently
turns into "not found" instead of a compile-time-obvious change.

Discovery earns its cost when the cell genuinely does not know what it will find: a logger, a
diagnostic, a bridge that forwards whatever a node produces, or a cell you intend to deploy
across boards that differ. For everything else, knowing the name is better than finding it.
