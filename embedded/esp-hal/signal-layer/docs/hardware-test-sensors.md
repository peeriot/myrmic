# Hardware test guide — Signal Layer sensor pipeline (ESP32-C6)

End-to-end procedure to take a freshly built Signal Layer pipeline to real hardware
and observe its taps. Walks through wiring, regen, build, flash, and reading the
generated taps via the `signal-layer-logger` WASM cell.

> **Read side.** This guide covers sensors → taps. For the **write side** (actuators and
> outlets — relay, PWM, feedback), see the companion runbook
> [`hardware-test-actuators.md`](hardware-test-actuators.md).

## Prerequisites

- ESP32-C6-DevKitC-1 (or any ESP32-C6 board with GPIO10/GPIO11 broken out).
- A BME280 breakout board (or any sensor referenced by the active pipeline; the
  shipped `basic-sensors` pipeline uses BME280, VEML7700, and CCS811, plus a
  moving-average step on the BME280 temperature output).
- USB-C cable for power + serial.
- Toolchain installed:
  - `rustup toolchain install nightly` with the `riscv32imac-unknown-none-elf`
    target.
  - `cargo install espflash --locked`.
- `wamrc` on `$PATH` if you intend to (re)build the WASM cell that reads taps.

The host scripts under `sdk/signal-layer/scripts/` drive everything; you can
also run inside the Docker build env (`scripts/docker-buildenv.sh`).

## 1. Wire the sensor

The `esp32c6-devkit` board manifest (at
`embedded/esp-hal/signal-layer/boards/esp32c6-devkit.yaml`) maps the I²C bus as:

| Manifest pin | ESP32-C6 GPIO | Sensor pin |
| ------------ | ------------- | ---------- |
| `i2c0.scl`   | GPIO10        | SCL        |
| `i2c0.sda`   | GPIO11        | SDA        |

Wire the sensor's `VCC` to 3V3 and `GND` to GND on the devkit. The bus runs at
400 kHz with 4.7 kΩ pull-ups on most breakout boards already; add external
pull-ups if your sensor lacks them.

If you change the wiring, edit the board manifest's `buses.i2c0.pins` — codegen
will pick up the new pins and re-emit `pipeline_pins!` so the WASM `Pins` set
stays consistent.

## 2. Build the firmware

```bash
cd sdk/signal-layer
scripts/build.sh basic-sensors --target esp32c6
```

`build.sh` runs `pipeline_regen.sh` first (regenerating
`embedded/esp-hal/modem-esp32/src/pipeline_config.rs` from the devkit manifest
and the `basic-sensors` pipeline — that file is gitignored as a build artifact)
and then `cargo +nightly build-c6` from the repo root. The resulting binary is
at `target/riscv32imac-unknown-none-elf/release/modem-esp32`. Re-run `build.sh`
any time you change the pipeline YAML, the manifest, a driver/step descriptor,
or the codegen itself.

To inspect what codegen produced:

```bash
grep -E "TAP_|fn .*_task|pipeline_pins" \
    ../esp-hal/modem-esp32/src/pipeline_config.rs
```

You should see one `TAP_*` static per tap (plus `TAP__SIGNAL_LAYER_HEALTH`), one
`*_task` per source, and the `pipeline_pins!` macro reserving the bus pins.

To build a no-pipeline firmware (useful for isolating WASM-only issues), drop
the `pipeline-basic-sensors` feature from `embedded/esp-hal/modem-esp32/Cargo.toml`'s
default `esp32c6` feature, or call cargo directly with
`--no-default-features --features esp32c6` (without `pipeline-*`). The WASM
runtime will then own GPIO10/11 too.

## 3. Flash and open the serial monitor

```bash
scripts/flash.sh --monitor
```

Defaults: `esp32c6` target, espflash auto-detect for the serial port. Pass
`--port /dev/ttyUSB0` if auto-detect fails. The `--monitor` flag opens
`espflash`'s serial monitor after flashing.

What to expect in the boot log:

```
INFO  - Init!
INFO  - [tap] Registry initialised (8 taps)        # 1 health + 7 pipeline taps
INFO  - [bme280] init OK at 0x76
INFO  - [veml7700] init OK at 0x10
INFO  - [ccs811] init OK at 0x5A
```

If the BME280 init fails (e.g. wiring), the per-source task emits a
`DriverHealth::Down` on `_signal_layer_health` and **keeps retrying** `init()`
on the sample-interval cadence — it does not exit. Once the sensor responds, the
task brings it up and emits `Up`. The VEML7700 task is independent and keeps
running throughout. There's no per-tick health logging — only transitions are
logged.

## 4. Read the taps from a WASM cell

The `signal-layer-logger` cell in `tests/fixtures/signal-layer-logger/` walks
the tap registry every second and prints each tap's name, kind, and decoded
value — a quick way to see what the running pipeline is producing.

> **Deploying a cell.** The automated path is the HIL suite: it brings up a
> local swarm and deploys a cell over Zenoh onto the device. The signal-layer
> HIL tests (`embedded/hil-tests/tests/integration/signal_layer/`) do exactly
> this against the `hil-tests` pipeline — see `embedded/hil-tests/README.md`
> ("Signal-layer (dataplane) tests") for the build + deploy + run sequence. They
> deploy a `tap-bridge` cell that reads taps over Zenoh so assertions are
> automated rather than eyeballed on the serial console.

Once the cell is running, the monitor shows lines like:

```
[signal-layer-logger] temperature     (retained f32): 22.34
[signal-layer-logger] humidity        (retained f32): 41.20
[signal-layer-logger] avg_temperature (retained f32): 22.31
```

The cell is sensor-agnostic — it lists everything the manifest's pipeline
registered. Add a sensor to the pipeline YAML, rebuild, and the new tap shows
up without touching the cell.

## 5. Verify health behaviour

Unplug the BME280's SDA wire while the firmware is running. Within one sample
interval (1000 ms by default) you should see:

```
[signal-layer-logger] _signal_layer_health: HealthEvent { source: 0, state: Degraded }
```

At the same time the BME280's retained taps (`temperature`, `humidity`,
`pressure`, `avg_temperature`) are **cleared** — the logger stops printing a
value for them (the slots read empty) instead of repeating the last-good
reading. The task then re-runs `init()` each tick to recover; if `init()` keeps
failing it transitions to `Down`.

Reconnect — the next successful `init()` + sample brings the sensor back, the
cell logs:

```
[signal-layer-logger] _signal_layer_health: HealthEvent { source: 0, state: Up }
```

and the retained taps repopulate. Repeated failures while already
`Degraded`/`Down` do not re-emit (transition guard in the generated per-source
task). This is the contract specified by the driver-health story.
