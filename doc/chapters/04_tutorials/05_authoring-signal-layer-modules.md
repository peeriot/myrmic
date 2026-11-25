# Authoring Signal Layer modules

The Signal Layer ships drivers and steps for common parts, but sooner or later you will want one
it does not have: a sensor nobody has written a driver for, or a calculation no shipped step does.
This tutorial writes both, a step and a driver, and uses them in a pipeline without touching the
Myrmic repository.

`myrmic signal-layer` scaffolds each module as a small standalone crate, already compiling and with its
tests passing, so the work is filling in the parts only you know: the arithmetic for a step, the
register map for a driver.

## What you build

You create a small module registry of your own, a directory holding `drivers/` and `steps/`, and
put two modules in it:

- a **step** that transforms a value on its way through a pipeline, and
- a **driver** for an I2C sensor.

Then you point a Linux pipeline at that registry, wire both modules in, and run it. The step runs
live over the synthetic `sim-source` from [First Steps](04_first-steps-signal-layer.md); the driver
is written and unit-tested here, and reading it live is a matter of having the sensor.

This is the path for authoring a module locally, before it is ready to move into the Myrmic tree or
into a shared module library. Nothing here changes your checkout.

## What you need

- The `myrmic` CLI and the Rust toolchain, per [Installation](../01_quickstart/01_installation.md).
- A clone of the Myrmic repository, so the scaffolds can resolve the SDK. Clone it and point
  `myrmic signal-layer` there once:

  ```bash
  git clone https://github.com/peeriot/myrmic.git ~/myrmic
  export PEERIOT_MYRMIC_SDK=~/myrmic   # the path to your clone
  ```

  Or pass `--sdk ~/myrmic` to `myrmic signal-layer step` and `myrmic signal-layer new`; a driver
  crate depends on nothing from the SDK, so `driver` has no such flag. The flag wins if you set both.
- A Linux machine. Everything here builds and runs with no attached hardware: the step is pure
  arithmetic, the driver is tested against a mock bus, and the pipeline reads the synthetic
  `sim-source`.

You do not need to have done [First Steps](04_first-steps-signal-layer.md) first, but it is the
gentler introduction to pipelines, taps and `sim-source`, and this tutorial reuses all three.

## The parts

1. [A step](05_authoring-signal-layer-modules/01_a-step.md): scaffold a processing step, learn the
   `ProcessingStep` contract, and test it.
2. [A driver](05_authoring-signal-layer-modules/02_a-driver.md): scaffold an I2C sensor driver,
   fill in its register map, and test it against a mock bus.
3. [Using them](05_authoring-signal-layer-modules/03_using-them.md): point a pipeline at your
   registry, wire both modules in, and build and run it.

The [Write your own driver](../05_guides/11_signal-layer/07_write-your-own-driver.md) and
[Write your own step](../05_guides/11_signal-layer/08_write-your-own-step.md) guides are the
reference for everything this tutorial does by example.
