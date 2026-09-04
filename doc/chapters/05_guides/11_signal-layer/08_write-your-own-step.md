# Write your own step

A step is a small Rust crate in a checkout of the `myrmic` repository, a directory under
`signal-modules/steps/` next to the shipped ones. Start its `src/lib.rs` with
`#![cfg_attr(not(test), no_std)]`: every shipped step has it, and without it your crate builds on
your laptop and fails when someone compiles it for a board.

A step is a transform. It takes a value on its way through the pipeline, does something to it,
and passes on the result: smoothing a noisy reading, converting a unit, turning a temperature
into a fan speed, turning a threshold crossing into an event.

If a driver is what knows a device, a step is what knows a calculation. It never touches
hardware, and it never talks to anything outside itself.

## The contract

A step is one trait:

```rust
pub trait ProcessingStep {
    type Input;
    type Output;
    fn step(&mut self, input: Self::Input) -> Option<Self::Output>;
}
```

That is the whole interface. A value goes in, and either a value comes out or nothing does.

Three constraints come with it, and they are the reason steps are cheap to run and easy to test.
A step is **synchronous**: no awaiting, because it is called inline on the source's tick. It has
**no access to the outside**: no slots, no registry, no shared state, no clock. And it is
**self-contained**: everything it needs is its own state and the value it was handed.

What a step *may* do is keep state, and most interesting ones do. The trait takes `&mut self`
precisely so a step can remember something between calls: a window of past samples, a counter, a
flag for whether it has already fired.

### A step must never block

This is the constraint with teeth, so it is worth being blunt about the consequence.

A step runs inline on its source's task, and on a microcontroller that task shares the main
executor with the rest of the pipeline, the cell runtime and the watchdog's feeder. The feeder's
scheduling *is* the liveness signal: if that executor stops turning, the feed stops with it.

So a step that spins, sleeps, or grinds through a long computation does not merely delay its own
pipeline. The executor is cooperative, so a task that never yields is never preempted: everything
sharing it stalls, including the heartbeat the watchdog depends on, so the feed is withheld and
the device resets.

On Linux the outcome is milder but still bad: the step occupies a runtime worker for as long as
it runs, and there is no watchdog to rescue anything.

Keep `step` to arithmetic over its input and its own state. If a calculation is genuinely
expensive, do it in a cell, where taking a while costs only that cell.

## Returning nothing is a normal answer

`Option<Output>` is not an error channel. It means *no value this time*, and it is how a step
says "ask me again later".

A moving average returns nothing until its window has filled. A cadence step returns nothing on
the samples it is thinning out. An edge trigger returns nothing on every sample that is not a
crossing, which is most of them.

When a step returns `None`, whatever it feeds is simply not updated: the next step is not called,
and a tap downstream keeps whatever it had rather than being cleared. So returning nothing is
quiet, not disruptive, and you should reach for it whenever the honest answer is that you do not
have one.

## A worked step

Here is a whole step, config and all:

```rust
use signal_layer_core::ProcessingStep;

pub struct ScaleConfig {
    pub factor: f32,
    pub offset: f32,
}

pub struct ScaleState {
    factor: f32,
    offset: f32,
}

impl ScaleState {
    pub fn new(cfg: ScaleConfig) -> Self {
        Self { factor: cfg.factor, offset: cfg.offset }
    }
}

impl ProcessingStep for ScaleState {
    /// What the step consumes.
    type Input = f32;
    /// What it emits.
    type Output = f32;

    fn step(&mut self, input: Self::Input) -> Option<Self::Output> {
        let output = input * self.factor + self.offset;
        Some(output)
    }
}
```

The two types do not have to match. A step that changes the type is ordinary; the shipped `fan-curve` takes an `f32` reading and emits a `PwmDuty` outlet value (see the [steps reference](../../10_reference/04_signal-layer/04_steps.md)), and the pipeline checks the chain end to end at generation time.

The split into a `Config` and a `State` is the convention: the config is what the pipeline
declares, the state is what runs. A step that never returns `None` is fine, as this one is; a
step whose answer is not always available returns `None` on the calls where it has none.

## The descriptor

As with drivers, the generator reads a descriptor and never your source:

```yaml
id: scale
category: processing-steps
description: >
  Linear transform: multiplies the input by `factor` and adds `offset`. Emits on
  every sample; there is no warm-up and no state carried between samples.
inputs:
  - name: value
    type: f32
outputs:
  - name: scaled
    type: f32
config_schema:
  factor:
    scope: application
    rust_type: f32
    default: 1.0
    description: Multiplier applied to the input
  offset:
    scope: application
    rust_type: f32
    default: 0.0
    description: Added after scaling
```

`inputs` and `outputs` are how the generator type-checks the wiring. If a pipeline feeds your
step something that does not match its declared input, generation fails and names both types,
which is a far better outcome than a type error surfacing at runtime.

Step config is always `scope: application` in practice, and should be. A step is arithmetic, and
arithmetic does not depend on how the board is wired, so there is nothing for the board file to
say. Note this one is a convention rather than a rule: unlike driver config, a step's scope is not
enforced, and a step declaring `hardware` would still be read from the pipeline. If you find
yourself wanting it, that is a hint the value belongs to a driver instead.

Write the description as if it will be the only thing a reader sees, because on the [steps reference](../../10_reference/04_signal-layer/04_steps.md) it is. Say what the step computes,
what each config field does in behavioural terms, and what it emits when it has no answer yet.

## Types, and when to be generic

Most steps name concrete types, as the example does: `f32` in, `f32` out.

They do not have to match. A step is free to change the type, and some of the more useful ones
do: a threshold turns a reading into an event, a curve turns a measurement into a control value.
Type conversion is a perfectly good reason for a step to exist.

A few steps genuinely do not care what they are carrying. Cadence is one: it decides *whether* to
pass a value along, never what the value is, so it is generic and declares no fixed types. That
is worth copying when it is true, and worth avoiding when it is not. Being generic to seem
flexible costs you the type check that would have caught a mis-wired pipeline.

## Adding it to the tree

Step crates live beside the others under `signal-modules/steps/<id>/`, and a step's crate is
named exactly its `id`, with no suffix. That differs from drivers, which are always
`{id}-driver`, so it is worth checking rather than assuming.

Once the crate and its descriptor are in place, a pipeline can name it as an `op` and wire it to
anything whose type matches.

## Testing

Steps are the easiest thing in the Signal Layer to test, because they are functions with memory
and nothing else. No fake hardware, no bus, no pipeline: construct the state and call it.

```rust
let mut s = ScaleState::new(ScaleConfig { factor: 2.0, offset: 1.0 });
assert_eq!(s.step(3.0), Some(7.0));
```

For a step with state, the test that matters is the sequence rather than any single call: that a
moving average stays quiet until its window fills, that an edge trigger fires once and not again
while the condition holds, that a cadence passes exactly one sample in every four. Those are the
behaviours a description promises, and they are cheap to pin down.

If you find a step hard to test, it is usually a sign it is doing something a step should not:
reaching for time, for a slot, or for the outside world. That work belongs in a driver or a cell.
