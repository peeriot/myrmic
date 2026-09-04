# Signal Layer — ESP32

The ESP32-specific half of the Signal Layer. The chip-agnostic crates (drivers, steps, tap
registry, and the `pipeline-codegen` library) live under
[`sdk/signal-layer/`](../../../sdk/signal-layer/); this directory holds the ESP32 pieces that turn
them into firmware:

- **`esp-codegen/`** — the `esp-codegen` binary. Implements the `ChipBackend` seam from
  `pipeline-codegen` (ESP peripheral construction, imports, bus types) and drives generation:
  a board manifest + a pipeline YAML → the `pipeline_config.rs` the `modem-esp32` firmware
  includes.
- **`boards/`** — **board manifests**: pure hardware descriptions of a specific device (buses,
  pins, ADCs, which sensors are wired where, hardware-scope config like I²C addresses). One
  file per board.
- **`pipelines/`** — **pipeline definitions**: the declarative `sources → steps → taps`
  graph plus application-scope config (sample intervals, tuning). One file per firmware
  behaviour.

A build pairs one board manifest with one pipeline. The two config scopes are kept apart —
setting a hardware field from a pipeline (or vice versa) fails the build.

## Boards

| Manifest                              | Board                                      |
| ------------------------------------- | ------------------------------------------ |
| `boards/esp32c5-devkit.yaml`          | ESP32-C5 DevKit.                           |
| `boards/esp32c6-devkit.yaml`          | ESP32-C6 DevKit.                           |
| `boards/esp32c61-devkit.yaml`         | ESP32-C61 DevKit.                          |
| `boards/esp32c5-hil.yaml`             | The C5 hardware-in-the-loop test rig.      |
| `boards/esp32c6-hil.yaml`             | The C6 hardware-in-the-loop test rig.      |
| `boards/esp32c61-hil.yaml`            | The C61 hardware-in-the-loop test rig.     |

## Pipelines

| Pipeline                              | Demonstrates                                          |
| ------------------------------------- | ----------------------------------------------------- |
| `pipelines/basic-sensors.yaml`        | Sensors → taps (read side).                           |
| `pipelines/actuators-demo.yaml`       | Cell-driven outlets → actuators (write side).         |
| `pipelines/feed-forward-demo.yaml`    | Feed-forward control (e.g. fan-curve).                |
| `pipelines/feedback-demo.yaml`        | Outlet feedback: a cell reads an actuator's real state back, not just its command. |
| `pipelines/hil-tests.yaml`            | Pipeline used by the HIL test suite.                  |

## See also

- [`sdk/signal-layer/README.md`](../../../sdk/signal-layer/README.md) — the chip-agnostic
  crate map.
- [the Signal Layer guide](../../../doc/chapters/05_guides/11_signal-layer.md) and
  [reference](../../../doc/chapters/10_reference/04_signal-layer.md) — the model, the file
  formats, and the module catalogues.
- [`docs/hardware-test-sensors.md`](docs/hardware-test-sensors.md) — read-side hardware runbook:
  regen, build, flash, read taps, verify driver health.
- [`docs/hardware-test-actuators.md`](docs/hardware-test-actuators.md) — write-side runbook:
  outlets, PWM/relay, feedback.
