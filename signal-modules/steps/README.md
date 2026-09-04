# Processing steps

One crate per processing step. A step is a small `#![no_std]` crate implementing the
`ProcessingStep` trait: a pure, synchronous transform that reads one input value and
optionally emits a derived value or event. Steps sit between a driver and a tap in a pipeline
— no `.await`, no slot access, no per-step task. Each crate ships a `descriptor.yaml`
(`category: processing-steps`) that codegen reads to wire it in.

For the trait contract and how to add one, see the "Adding a processing step" section of the
parent [`../README.md`](../README.md).

## Available steps

| Crate                                       | Transform                                                                    |
| ------------------------------------------- | ---------------------------------------------------------------------------- |
| [`moving-average`](moving-average/)         | Windowed moving average; emits once the window fills.                        |
| [`max-value`](max-value/)                   | Running maximum of the input.                                                |
| [`min-value`](min-value/)                   | Running minimum of the input.                                                |
| [`threshold-trigger`](threshold-trigger/)   | Emits an alarm **event** when the input crosses a threshold.                 |
| [`hysteresis`](hysteresis/)                 | Two-threshold hysteresis controller for feed-forward actuator control.       |
| [`fan-curve`](fan-curve/)                   | Feed-forward transfer function mapping an input reading to an actuator command.|
| [`cadence`](cadence/)                       | Rate control: pass one of every N samples (decimate or sample-and-hold).     |
