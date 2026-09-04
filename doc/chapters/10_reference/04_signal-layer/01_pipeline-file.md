# Pipeline file

The pipeline file says what a node does with its hardware: which devices produce readings, what
happens to those readings, and what a cell can see and change. It names devices, not pins, so the
same pipeline runs against any board whose devices match.

```yaml
pipeline:
  id: basic-sensors

sources: []
steps: []
taps: []
outlets: []
```

Only `pipeline.id` is required. The four lists all default to empty.

## `sources`

Binds a device from the board file and runs it.

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | Name used to refer to this source's readings |
| `device` | yes | A device `id` from the board file |
| `config` | no | Settings the driver declared as `scope: application` |

`sample_interval_ms` is accepted in `config` on every sensor source. It is not part of the
driver's own configuration: the pipeline uses it to set the loop rate. When it is omitted, the
driver's descriptor default applies, which differs per device.

## `steps`

Transforms a value on its way through.

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | Name used to refer to this step's output |
| `op` | yes | Which step module, from the [steps reference](./04_steps.md) |
| `input` | yes | What feeds it |
| `config` | no | Settings the step declared |

A step has exactly one input. Several steps may share one input, so a value can fan out.

## `taps`

Publishes a value under a name a cell can resolve.

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | The name a cell resolves |
| `kind` | yes | `retained` or `event` |
| `type` | yes | The declared payload type |
| `source` | yes | What produces the value |

`retained` holds the latest value with a timestamp and reading does not consume it. `event` is a
queue the reader drains, and carries no timestamp.

A third kind, `batch`, is reserved for planned work. Declaring one fails generation with
`batch taps not yet supported by codegen`.

## `outlets`

Exposes a value a cell, or a step, can write.

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | The name a cell resolves |
| `type` | yes | The value type the driver accepts |
| `device` | yes | A device `id` from the board file |
| `input` | no | A step whose output drives this outlet with no cell involved |
| `config` | no | Settings for the outlet |

`write_interval_ms` in `config` sets how often the device's task applies the latest value.
It defaults to **100 ms** and must be at least 1.

A device may be driven by exactly one outlet.

## What can be referenced

Wherever a `source:` or an `input:` is expected, one of these forms is accepted:

| Form | Refers to |
|---|---|
| `<source id>.<output name>` | one of a driver's published outputs |
| `<step id>` | the output of a step |
| `<outlet name>.<status field>` | a read-back from a device that reports its state |
| `<outlet name>.error` | faults from that outlet, as `OutletFault` |

## Payload types

These are the type names accepted in a tap's or outlet's `type` field.

| Type | Used for |
|---|---|
| `f32`, `f64` | measurements |
| `u8`, `u16`, `u32`, `u64`, `usize` | counts and raw values |
| `i32`, `i64` | signed values |
| `bool` | on/off state, such as a contact read-back |
| `DigitalState` | value for an on/off output |
| `PwmDuty` | value for a duty-cycle output |
| `ThresholdAlarm` | event from a threshold step |
| `OutletFault` | event from an outlet's `.error` |
| `HealthEvent`, `DriverHealth` | source health, on `_signal_layer_health` |

A tap's declared type is checked twice: against what produces it during generation, and against
what a cell asks for at runtime. The typed SDK calls compare their type with the declared one
before decoding and fail with a type-mismatch error; payloads are decoded strictly, refusing
trailing bytes. Only the raw byte reads and writes bypass the cell-side check.

## Limits

| Limit | Value |
|---|---|
| Taps per pipeline | 16, of which 1 is reserved for `_signal_layer_health` when the pipeline has any source |
| Outlets per pipeline | 8 |
| Events held per event tap | 8, oldest dropped when full |

## Reserved names

Names beginning with `_` are reserved for taps the generator injects. Today that is
`_signal_layer_health`, added automatically to any pipeline with at least one source.
