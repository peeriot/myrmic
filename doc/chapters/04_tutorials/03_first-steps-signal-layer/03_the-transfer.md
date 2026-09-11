# Part 3 - The Transfer

This is Part 3 of [First Steps with the Signal Layer](../03_first-steps-signal-layer.md), and the
reason the first two exist. The pipeline and the cell are running on the Linux machine. Now the
same pipeline goes onto an ESP32-C6, and the same cell moves onto it, unchanged.

Make sure the embedded prerequisites from the
[tutorial's intro](../03_first-steps-signal-layer.md) are installed: nightly Rust
(`nightly-2026-08-07`) with `rust-src`, `riscv32imac-unknown-none-elf` target and `wamrc` 2.4.4 on
your PATH.

---

## Step 1 - Scaffold the firmware pipeline

On embedded, the pipeline is compiled *into the firmware* rather than run as its own process.
`myrmic new --firmware` scaffolds that firmware, and `--pipeline` gives it a Signal Layer pipeline:

```bash
myrmic new --firmware=esp32c6 --pipeline ~/sl-tutorial/first-steps-c6
cd ~/sl-tutorial/first-steps-c6
```

As in Parts 1 and 2, this uses the `PEERIOT_MYRMIC_SDK` checkout from the
[prerequisites](../03_first-steps-signal-layer.md); add `--sdk <path>` if you did not set it.

The scaffold writes a `board.yml` for the C6 — pins instead of a device path:

```yaml
id: first-steps-c6
chip: esp32c6

buses:
  i2c0:
    transport: i2c
    pins:
      scl: 10
      sda: 11
    freq_khz: 400

gpios:
  general_purpose: [0, 1, 2, 3, 14, 18, 19, 20, 21, 22, 23, 27]

devices:
  - id: sim
    driver: sim-source
    bus: i2c0
```

Compare it to the Linux `board.yml` from Part 1. The chip changed, and the bus is reached by pins
instead of a device path. The device list is identical. On the ESP32 the bus id `i2c0` is not a
free name: it selects the chip's `I2C0` peripheral, and naming it `i2c1` fails the build, because
the C6 has no such peripheral.

The **pipeline is portable** — copy the `pipeline.yml` you finished in Part 1 over the scaffold's,
byte for byte:

```bash
cp ~/sl-tutorial/first-steps/pipeline.yml ~/sl-tutorial/first-steps-c6/pipeline.yml
```

That the same `pipeline.yml` drives both machines is the whole point.

## Step 2 - Build, flash, and watch it join

`myrmic flash` builds the firmware and writes it to the board; the WiFi credentials are baked in at
compile time, and `--monitor` streams the serial output afterwards:

```bash
export WIFI_SSID="your-network" WIFI_PASS="your-password"
myrmic flash ~/sl-tutorial/first-steps-c6 --monitor
```

The device finds the runtime the same way the CLI does: by scouting the local network. On an
ordinary flat network (both machines on one router) that simply works. If your network blocks
multicast, give the firmware the runtime's address instead — restart the Part 2 runtime with a
pinned port and set the address at build time:

```yaml
# ~/sl-tutorial/runtime.yml — restart the runtime with: myrmic runtimes start ~/sl-tutorial/runtime.yml
zenoh:
  listen:
    endpoints:
      peer: ["tcp/[::]:7447"]
```

```bash
export TCP_DIRECT_ADDR="192.168.178.109:7447"   # the Linux machine's address and pinned port
myrmic flash ~/sl-tutorial/first-steps-c6 --monitor
```

Expected in the serial output, in this order:

```text
INFO - [tap] Registry initialised (3 taps)
INFO - [sim-source] init OK (synthetic)
INFO - Wifi connected to ConnectedInfo { ssid: "your-network", ... }
INFO - clock synced to swarm time (...)
INFO - Registering exec runtime with info: ExecRuntimeInfo { ..., name: Some("ESP32-C6"), ... }
```

Read that log as the story it is: the same pipeline came up (two taps plus the health tap), the
same simulated sensor started, and then the device joined the swarm your Linux runtime is running
and offered itself as a place to run cells.

## Step 3 - Move the cell

The cell is still on the Linux runtime, and a cell exists in one place at a time. Deploy it again
under the same name, this time for the device platform `riscv32imac` (the ESP32-C6): that rebuilds
it and ahead-of-time compiles it with `wamrc`. The Linux runtime is still in the swarm and could
host the cell too, so `--tag esp32c6` pins the placement to the C6 and moves the cell onto it:

```bash
myrmic deploy ~/sl-tutorial/thermometer --name thermometer --platform riscv32imac --tag esp32c6
```

Nothing in `thermometer/src/lib.rs` changed. Look at where it landed now:

```bash
myrmic cells status
```

```text
  cell         sri           kind  runtime     age  policy  class        srn
  thermometer  e6f23498-...  aot   [7]96d60de  4s   never   thermometer  thermometer
```

Two things moved from the Part 2 row: the `kind` is now `aot` (ahead-of-time compiled for the
device, not `wasm`), and the `runtime` column is the ESP32's id, not the Linux machine's. That
column is the proof — the same cell is now running on the microcontroller.

Watch it read the device's own taps in the serial monitor:

```text
INFO - t=6992 value=40 avg=25
INFO - t=7992 value=60 avg=45
```

The cell you wrote for a Raspberry Pi is now reading a simulated sensor on an ESP32, over the same
tap names, having changed not one line. The board file was the only thing that differed between the
two machines, and the CLI generated everything else from it.

## What's next

- Swap `sim-source` for a real sensor: wire it to the bus, change the `driver:` and set the bus
  `dev_path` (Linux) or pins (embedded) in `board.yml`. The [Signal Layer
  guide](../../05_guides/11_signal-layer.md) walks through drivers, steps, actuators, and health.
- The full file formats are in the [Signal Layer reference](../../10_reference/04_signal-layer.md);
  the scaffolding flags are in [`myrmic new`](../../10_reference/02_myrmic-cli/01_new.md).
