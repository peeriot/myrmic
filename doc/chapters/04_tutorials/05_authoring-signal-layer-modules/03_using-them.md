# Part 3: Using them

You have two modules in `~/sl-modules`: the `deadband` step and the `thermo` driver. Now you point a
pipeline at that directory so its build picks them up, alongside the modules that ship with myrmic.
Nothing in your Myrmic checkout changes; the registry stays entirely your own.

## Scaffold a pipeline beside the registry

Create the pipeline next to `~/sl-modules`, so a short relative path reaches it:

```bash
cd ~
myrmic signal-layer new tut-pipeline
```

That is the standalone Linux pipeline from [First Steps](../04_first-steps-signal-layer.md): a
`board.yml`, a `pipeline.yml`, and a crate that builds them at compile time. Its `board.yml` already
declares a `sim-source` device, which is all the step needs.

## Point the build at your registry

The generated `build.rs` reads `board.yml` and `pipeline.yml` and, optionally, a directory of your
own modules. Give it your registry (its third argument is a path relative to the pipeline crate):

```rust
// tut-pipeline/build.rs
linux_codegen::build_pipeline("board.yml", "pipeline.yml", Some("../sl-modules"));
```

Then declare the two crates you will use as ordinary path dependencies, so they end up in the
binary. A driver crate is named `<id>-driver`; a step crate is named its bare `id`:

```toml
# tut-pipeline/Cargo.toml, under [dependencies]
deadband = { path = "../sl-modules/steps/deadband" }
thermo-driver = { path = "../sl-modules/drivers/thermo" }
```

That is the whole of the local-dir workflow: a registry directory of modules, named on the build,
and each crate a dependency. A firmware pipeline is identical, with `.include("../sl-modules")` on
its `esp_firmware_build::pipeline()` builder instead of the third argument.

## Wire in the step and run it

Add the step to `pipeline.yml`. It runs over the synthetic `sim` source, so this runs with no
hardware. A step is named on the pipeline by its descriptor `id` (`deadband`), and its output tap
reads the step by the `id` you give the step in the pipeline:

```yaml
pipeline:
  id: tut-pipeline

sources:
  - id: sim
    device: sim
    config:
      sample_interval_ms: 500
      start: 0.0
      step: 10.0
      max: 100.0

steps:
  - id: filtered
    op: deadband
    input: sim.value
    config:
      delta: 5.0

taps:
  - name: filtered
    kind: retained
    type: f32
    source: filtered
```

Build it with `myrmic build`, the same command as for a cell or firmware, and run the binary it
reports:

```bash
cd ~/tut-pipeline
myrmic build
export XDG_RUNTIME_DIR=/run/user/$(id -u)   # only if your shell has none
./target/release/tut-pipeline
```

```text
Pipeline `tut-pipeline` running. Press Ctrl-C to stop.
[INFO  sim_source_driver] [sim-source] init OK (synthetic)
```

Your `deadband` step is now compiled into a running pipeline, filtering the ramp `sim` produces and
serving the result as the `filtered` tap. A cell reads it exactly as it read the taps in First
Steps.

## Wire in the driver

The driver is the same, with one honest caveat: reading it live needs the sensor. Declare it in
`board.yml` on the I2C bus, at the address your wiring uses:

```yaml
# tut-pipeline/board.yml, under devices:
  - id: thermo
    driver: thermo
    bus: i2c0
    hardware:
      i2c_addr: 0x48
```

Add a source that reads it, and a tap for the temperature, to `pipeline.yml`:

```yaml
sources:
  # ... the sim source above, plus:
  - id: thermo
    device: thermo
    config:
      sample_interval_ms: 1000

taps:
  # ... the filtered tap above, plus:
  - name: temperature
    kind: retained
    type: f32
    source: thermo.temperature
```

`myrmic build` again. It compiles: codegen found `thermo` in your registry, checked the pipeline
against its descriptor, and wired `thermo.temperature` to the tap. Running it now opens
`/dev/i2c-1` and talks to the sensor, so it needs the real part on the bus; on a Raspberry Pi with
the sensor wired, this is a live temperature tap. Without the sensor, leave the `thermo` source out
and the pipeline runs on `sim` alone, as above.

## Where this goes next

You wrote a step and a driver, tested both at your desk, and ran them from a pipeline without
touching the Myrmic tree. When a module has earned its place, it moves into `signal-modules/` in the
repo (or a shared module library) and becomes available to everyone by name, with no registry path.
The [Write your own driver](../../05_guides/11_signal-layer/07_write-your-own-driver.md) and
[Write your own step](../../05_guides/11_signal-layer/08_write-your-own-step.md) guides cover that
step and the full module reference.
