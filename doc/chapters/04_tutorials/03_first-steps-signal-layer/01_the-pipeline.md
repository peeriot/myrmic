# Part 1 - The Pipeline

This is Part 1 of [First Steps with the Signal Layer](../03_first-steps-signal-layer.md). You
describe your machine and your dataflow in two small files, scaffold a pipeline process from
them, and watch it come alive. Everything happens on the Linux machine.

---

## Step 1 - Scaffold the project

`myrmic new --pipeline` creates a standalone Linux pipeline project — the two YAML files you will
edit, and a `build.rs` that generates the pipeline from them at build time. Point `--sdk` at your
repository checkout so the project builds against your local code:

```bash
myrmic new --pipeline ~/sl-tutorial/first-steps --sdk ~/myrmic
cd ~/sl-tutorial/first-steps
```

```text
INFO  Creating Linux pipeline 'first-steps'
```

## Step 2 - The board file

`board.yml` describes the physical machine: which chip, which buses, which devices. The scaffold
writes one for a Linux host with a synthetic sensor:

```yaml
id: first-steps
chip: linux

# I2C buses by their Linux character device.
buses:
  i2c0:
    transport: i2c
    pins: {}
    freq_khz: 400
    dev_path: /dev/i2c-1

gpios:
  general_purpose: []

devices:
  - id: sim
    driver: sim-source
    bus: i2c0
```

Three things worth pausing on:

- **`pins: {}` is required**, even though pins mean nothing on Linux. The schema is shared with
  the embedded platforms, where pins are the whole point.
- **The bus is named `i2c0` deliberately.** On Linux the name is free. On an ESP32 the bus id
  names the chip's peripheral, and the C6 has an `I2C0` and no `I2C1`. Keeping `i2c0` here keeps
  the two board files parallel, and Part 3 will thank you.
- **`sim-source` is a real driver that fakes a sensor.** It emits a deterministic climbing value
  and never touches the bus it is bound to. It still must be bound to one in the file, but nothing
  opens that bus unless a driver actually transacts on it — and `sim-source` never does, so the
  pipeline runs with no I²C hardware and no `/dev/i2c-*` node.

## Step 3 - The pipeline file

`pipeline.yml` describes the dataflow, and unlike the board file it is portable. The scaffold
starts you with one source and one tap; add a `moving-average` step and a second tap so there is
something to smooth. The finished file:

```yaml
pipeline:
  id: first-steps

sources:
  - id: sim
    device: sim
    config:
      sample_interval_ms: 500
      start: 0.0
      step: 10.0
      max: 100.0

steps:
  - id: avg
    op: moving-average
    input: sim.value
    config:
      window: 4

taps:
  - name: sim_value
    kind: retained
    type: f32
    source: sim.value

  - name: sim_avg
    kind: retained
    type: f32
    source: avg
```

One source, sampled twice a second, climbing in steps of ten and wrapping at a hundred. One step,
averaging the last four samples; a step's output is named by its `id`, so the second tap reads
`source: avg`. Two taps: the raw value and the smoothed one. Note that the pipeline references the
*device* `sim` from the board file, never the bus; that is what will let it move to another machine
unchanged.

## Step 4 - Build and run it

The generated project is an ordinary Cargo binary — `build.rs` turns `board.yml` + `pipeline.yml`
into the pipeline at compile time:

```bash
cargo run
```

```text
   Compiling first-steps v0.1.0 (~/sl-tutorial/first-steps)
    Finished `dev` profile [unoptimized + debuginfo] target(s)
     Running `target/debug/first-steps`
Pipeline `first-steps` running. Press Ctrl-C to stop.
[INFO  sim_source_driver] [sim-source] init OK (synthetic)
```

If instead `cargo run` panics with `no socket path available`, your shell has no
`XDG_RUNTIME_DIR`. It is set on a desktop login but often unset over SSH or on a headless box (a
Raspberry Pi included). Set it and re-run — and because the runtime in Part 2 reaches the pipeline
over the same socket, set it the same in every terminal you use here (or add the line to your shell
profile):

```bash
export XDG_RUNTIME_DIR=/run/user/$(id -u)
```

The pipeline is now sampling the simulated sensor twice a second, feeding the average, and serving
both taps on a Unix socket at `$XDG_RUNTIME_DIR/peeriot-signal-layer.sock`. Nothing is reading them
yet. Leave it running; that is Part 2's job.
