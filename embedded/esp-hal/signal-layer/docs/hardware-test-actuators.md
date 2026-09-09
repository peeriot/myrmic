# Signal Layer actuators — hardware test runbook (ESP32-C6 DevKit)

Bench validation for the write-side (Outlet) feature on an ESP32-C6-DevKitC-1.
Each test uses a committed demo pipeline; regenerate + flash, then observe.

> **Write side.** This runbook covers actuators and outlets. For the **read side** (sensors →
> taps, driver health), see [`hardware-test-sensors.md`](hardware-test-sensors.md).

## Prerequisites

- ESP32-C6 DevKit, USB.
- `espflash` (the cargo runner is already `espflash flash --monitor`).
- A BME280 on I2C0 (**SCL = GPIO10, SDA = GPIO11**, addr `0x76`).
- An oscilloscope (or LED + resistor) for the output pins; a jumper wire for the feedback test.

Board pin map (`../boards/esp32c6-devkit.yaml`):

| Device | Driver | Pin(s) |
|--------|--------|--------|
| `relay1` | gpio-output | out = **GPIO2** |
| `fan1` | pwm-output | out = **GPIO3** (25 kHz PWM) |
| `relay_fb` | gpio-output-feedback | out = **GPIO14**, feedback = **GPIO18** |
| `bme280` | bme280 | I2C0 |

Build + flash a pipeline (from the repo root):

```sh
SIGNAL_LAYER_PIPELINE=../signal-layer/pipelines/<pipeline-name>.yaml \
    cargo +nightly run-c6 --features pipeline    # flashes + opens the serial monitor
```

The firmware's `build.rs` generates the pipeline into `OUT_DIR` at build time;
`SIGNAL_LAYER_PIPELINE` (a path relative to `embedded/esp-hal/modem-esp32`, or
absolute) selects it, and the board defaults to this chip's devkit. `run-c6` =
`run -p modem-esp32 --release --target riscv32imac-unknown-none-elf
--no-default-features --features esp32c6 -Zbuild-std=core,alloc`.

---

## Test 1 — BME280 → PWM fan + relay (feed-forward, no cell) ★ headline

**Pipeline:** `feed-forward-demo` · **Wiring:** scope on GPIO3 (PWM) and GPIO2 (relay).

```sh
SIGNAL_LAYER_PIPELINE=../signal-layer/pipelines/feed-forward-demo.yaml \
    cargo +nightly run-c6 --features pipeline
```

Warm the BME280 (finger/breath). Expect:
- **GPIO3**: a 25 kHz square wave whose **duty rises with temperature** — 0 % at ≤25 °C, 100 % at ≥40 °C (linear `fan-curve`).
- **GPIO2**: **toggles high at 30 °C, low at 27 °C** with hysteresis (no chatter in the band).
- Duty/relay recompute every ~2 s (`cadence every: 4` × 500 ms sampling, `SampleHold`).

Exercises: PWM + GPIO output drivers, in-layer feed-forward (`fan-curve`, `hysteresis`),
`cadence`, and the driver protective limits — **entirely autonomous, no WASM cell**.

## Test 2 — protective PWM update floor

Edit `fan1`'s `hardware.min_update_interval_ms` (e.g. `500`) in the board manifest, then
rebuild + reflash (cargo re-generates on the manifest change). Scope GPIO3: duty now steps at
most every 500 ms even as temperature moves — the driver floor overrides pipeline cadence.

## Test 3 — cell-driven outlets (needs a WASM cell)

**Pipeline:** `actuators-demo` (relay1 + fan1 both cell-driven). A cell drives them via the
SDK Outlet API:

```rust
use myrmic_sdk::Outlet;
use myrmic_sdk::signal_layer::{DigitalState, PwmDuty};

let fan = Outlet::resolve("fan1_duty")?.unwrap();
fan.write_typed(&PwmDuty { duty: 0.5 })?;        // 50 % PWM on GPIO3
let relay = Outlet::resolve("relay1_cmd")?.unwrap();
relay.write_typed(&DigitalState { on: true })?;  // GPIO2 high
```

The outlet names come from the pipeline, not the device ids: `actuators-demo` declares
`relay1_cmd` and `fan1_duty` (see [`../pipelines/actuators-demo.yaml`](../pipelines/actuators-demo.yaml)).
`feedback-demo` names its outlet `relay_cmd` instead, so the two demos are not interchangeable.

Scope GPIO3/GPIO2 as the cell writes. Exercises the cell-driven path + host `outlet_write_retained` +
single-writer ownership.

## Test 4 — hybrid feedback (needs a cell + a jumper) — #1018

**Pipeline:** `feedback-demo`. **Jumper GPIO14 → GPIO18.**

A cell writes `relay_cmd` and reads the `relay_contact` tap (via the normal Tap API). Because
GPIO14 is jumpered to the feedback input GPIO18, `relay_contact` reflects the **real pin
read** — flip the command and watch the tap follow. Remove the jumper (or pull GPIO18 the
other way) and the tap no longer matches the command, proving status is a genuine read-back,
not inferred from the write. Forcing a read/apply failure emits an `OutletFault` on the
`relay_fault` event tap.

## Test 5 — static pin mutual-exclusion (build-time)

In the board manifest, give a second device the same GPIO as `relay1` (`out: 2`), then
rebuild. Codegen (in `build.rs`) **fails** with "GPIO2 is claimed by both …" — the pin can be
owned by the pipeline or the cell GPIO host function, never both (resolved at codegen time).

---

## Notes / caveats

- **No safe-state on crash**: GPIO/PWM state during a panic or the watchdog reset window is
  undefined (SDS constraint 6). Don't rely on outputs for anything safety-critical.
- **Soft real-time**: in-layer control is best-effort feed-forward (threshold/hysteresis/
  curve), preempted by radio threads — no bounded control-loop timing.
- The pipeline is generated into the firmware's `OUT_DIR` at build time from the board manifest
  and the pipeline named by `SIGNAL_LAYER_PIPELINE`; nothing is written into the source tree, so
  there is no generated file to commit.
