# Part 1: A step

A step is a transform: a value goes in, and either a value comes out or nothing does. It knows a
calculation, never a device, and it keeps only its own state. That makes it the easiest module to
write, so it is where we start.

## Scaffold it

Pick a directory to keep your modules in; it does not have to exist yet. The `step` command
creates it and places a step under `steps/<id>` inside it, which is the layout a pipeline will
later read:

```bash
myrmic signal-layer step deadband --registry-dir ~/sl-modules
```

```text
INFO  Creating Signal Layer step 'deadband' in ~/sl-modules/steps/deadband
INFO  Add `deadband = { path = "~/sl-modules/steps/deadband" }` to a pipeline's dependencies and point its build.rs at `~/sl-modules` to build with it.
```

That second line is Part 3; ignore it for now. You have a standalone crate:

```text
~/sl-modules/steps/deadband/
    Cargo.toml
    descriptor.yaml
    src/lib.rs
```

It already compiles and its test passes. Prove it before you touch anything:

```bash
cd ~/sl-modules/steps/deadband
cargo test
```

## What the scaffold gives you

`src/lib.rs` opens with `#![cfg_attr(not(test), no_std)]`: a step runs on a microcontroller too, so
it cannot use the standard library. Below that is the shape every step has.

- A **`DeadbandConfig`**: the knobs a pipeline sets, one field per `config_schema` entry in
  `descriptor.yaml`.
- A **`DeadbandState`**: the step's own memory. The naming is a convention, the config is what the
  pipeline declares and the `State` is what runs; codegen instantiates `DeadbandState` (the `State`
  suffix is how it finds it).
- An **`impl ProcessingStep for DeadbandState`**: the whole interface.

```rust
pub trait ProcessingStep {
    type Input;
    type Output;
    fn step(&mut self, input: Self::Input) -> Option<Self::Output>;
}
```

The `Option` is the important part. Returning `None` is not an error; it means "no value this
time", and whatever the step feeds simply is not updated. A step that returns `None` until it has
something worth emitting is doing exactly what it should.

## Make it your own

The scaffold ships a rolling-average example. Replace `src/lib.rs` with a *deadband*: a step that
passes a value through only when it has moved by at least `delta` since the last one it emitted, and
stays quiet otherwise. It keeps one thing in state (the last value it emitted) and uses `None` for
every sample that has not moved far enough.

```rust
#![cfg_attr(not(test), no_std)]
//! deadband step: passes a value through only when it has moved by at least
//! `delta` since the last one it emitted, and stays quiet otherwise.

use signal_layer_core::ProcessingStep;

#[derive(Clone, Copy)]
pub struct DeadbandConfig {
    pub delta: f32,
}

impl Default for DeadbandConfig {
    fn default() -> Self {
        Self { delta: 1.0 }
    }
}

pub struct DeadbandState {
    delta: f32,
    last: Option<f32>,
}

impl DeadbandState {
    #[must_use]
    pub fn new(cfg: DeadbandConfig) -> Self {
        Self { delta: cfg.delta, last: None }
    }
}

impl ProcessingStep for DeadbandState {
    type Input = f32;
    type Output = f32;

    fn step(&mut self, input: f32) -> Option<f32> {
        let moved = match self.last {
            Some(last) => (input - last).abs() >= self.delta,
            None => true,
        };
        if moved {
            self.last = Some(input);
            Some(input)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_only_on_a_move_past_the_deadband() {
        let mut step = DeadbandState::new(DeadbandConfig { delta: 2.0 });
        assert_eq!(step.step(10.0), Some(10.0)); // the first sample always emits
        assert_eq!(step.step(11.0), None); // within the deadband, stays quiet
        assert_eq!(step.step(12.5), Some(12.5)); // moved by >= 2.0
        assert_eq!(step.step(12.6), None);
    }
}
```

The test is the point. A step's behaviour is a *sequence*, and the sequence is what a description
promises: quiet until the value moves, one emission on the move, quiet again. You pin that down at
your desk, with no pipeline and no hardware.

## Match the descriptor

Codegen never reads your Rust; it reads `descriptor.yaml`. The generated one describes the
rolling-average example, so update it to match the deadband: one `f32` input, one `f32` output, and
the `delta` knob. Every `config_schema` field maps to a `DeadbandConfig` field of the same name and
type.

```yaml
id: deadband
category: processing-steps
description: >
  Passes a value through only when it has moved by at least `delta` since the
  last one it emitted; stays quiet (emits nothing) otherwise.

inputs:
  - name: value
    type: f32

outputs:
  - name: result
    type: f32

config_schema:
  delta:
    scope: application
    rust_type: f32
    default: 1.0
    description: Minimum change from the last emitted value before emitting again
```

Run the test once more to confirm the edit compiles:

```bash
cargo test
```

That is a complete step. [Next](02_a-driver.md) you write a driver, which is more involved because
it talks to real hardware. If you only needed a step, skip to [Using them](03_using-them.md).
