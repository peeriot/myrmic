# HIL Tests

Hardware-in-the-loop integration tests for the embedded WASM runtime. They run on
your **host** with `cargo nextest`, drive a physically-attached **ESP32-C6** over
USB-serial, and exercise the full deploy path: a local swarm builds and
AOT-compiles a WASM cell, uploads it, and deploys it onto the device over Zenoh,
then asserts on the result.

The tests are gated behind the `EMBEDDED_TARGET` env var, which both enables them
and selects the chip under test. Without it every test prints "skipping" and
passes, so they are inert in normal CI — CI only *builds* the firmware, it never
runs these against hardware.

```sh
EMBEDDED_TARGET=ESP32C6 cargo nextest run -p hil-tests
```

A second var, `HIL_SENSORS`, says whether the rig has the board manifest's physical
sensors wired up. Four tests read a value out of real hardware and cannot pass
without them:

- `signal_layer::real_sensors::*` (3 tests)
- `signal_layer::steps::moving_average_produces_avg_temperature`, which averages the
  BME280 temperature

Those four skip themselves, with a logged reason, unless `HIL_SENSORS` is set. **A bare
devboard is a supported configuration, not a broken rig** — it runs the same firmware
and everything else passes, including `discovery` (the tap registry reflects the
compiled pipeline, not live hardware) and the `health::absent_sensor_*` tests, which
are *about* absent sensors. Set it on a fully populated rig:

```sh
EMBEDDED_TARGET=ESP32C6 HIL_SENSORS=1 cargo nextest run -p hil-tests
```

Note the signal-layer tests also need the pipeline compiled into the firmware; see
"Signal-layer tests" below. Wiring alone is not enough.

There are three toolchains involved: the **host** test harness, the **firmware**
(`modem-esp32`, riscv32 `no_std`), and the **WASM cell + AOT** path (`wamrc`).

---

## Agent-assisted setup

If you're driving this with a coding agent, read this first.

**Human-only steps.** The agent should hand these to you and wait:
- The `sudo` installs (system packages, `usermod -aG dialout`) — agents can't run
  interactive `sudo`.
- Attaching the board (and pressing BOOT if a flash ever needs it).
- The **firmware build** (step 5) — run it yourself, because `WIFI_PASS` is baked
  into the binary at compile time and you don't want your WiFi password captured
  into the agent's transcript. The agent can give you the exact command.
- Anything long-running or interactive: the serial **monitor** (streams until
  Ctrl-C) and the **test run** itself (heavy compile/link) are best run in your own
  terminal; the agent watches the output you paste back.

Everything else — auditing the host, building/installing `wamrc`, checking the
serial device and the firmware ELF — the agent can do on its own.

**Audit before installing.** Most of these prerequisites are commonly already
present. Detect first, then install only the gaps:

```sh
rustup toolchain list | grep nightly
rustup target list --installed --toolchain nightly   # want riscv32imac, riscv32imc, wasm32
rustup component list --toolchain nightly | grep 'rust-src.*installed'
cargo nextest --version
which wamrc && wamrc --version                        # want 2.4.4
id -nG | tr ' ' '\n' | grep -x dialout                # serial access
for p in build-essential clang libclang-dev llvm-dev cmake ninja-build pkg-config libssl-dev; do
  dpkg -s "$p" >/dev/null 2>&1 && echo "ok   $p" || echo "MISS $p"
done
ls -l /dev/ttyACM0 2>/dev/null                        # board present?
ls -l target/riscv32imac-unknown-none-elf/release/modem-esp32 2>/dev/null  # firmware built?
```

**Judge progress by greppable signals**, not prose. In a `--no-capture` run the
pipeline is healthy when these appear in order: `Using Zenoh node at` →
`Registering exec runtime` → `Compile success` (the `wamrc` AOT step) →
`Cell registered`. Only after those does the test's own assertion run — so an
assertion failure *after* all four is a behavior/test issue, not a setup problem.

---

## Setup (one-time, from a fresh machine)

Validated on Ubuntu 24.04 with an ESP32-C6 (built-in USB-Serial-JTAG, no external
probe). The canonical toolchain reference is the build-env Dockerfile at
`docker/images/myrmic-buildenv/Dockerfile`.

### Prerequisites

- An **ESP32-C6** and a USB cable.
- The laptop and the ESP32 must be on the **same WiFi / subnet**. The firmware
  joins WiFi, then **UDP-multicast-scouts** for the laptop's swarm. If your network
  blocks multicast or uses client isolation, set `TCP_DIRECT_ADDR=<laptop-ip>:<port>`
  at firmware build time to point the device straight at the laptop and skip
  scouting.
- **SSH access to Peeriot's private GitHub repos** — the firmware depends on
  private git deps (`wamr-rust-sdk`, the `zenoh` fork, etc.).

### 1. System packages

```sh
sudo apt-get install -y \
  build-essential clang libclang-dev llvm-dev \
  cmake ninja-build pkg-config libssl-dev
```

`clang`/`libclang-dev` are needed because the firmware compiles WAMR's C runtime;
`llvm-dev` + `cmake` + `ninja-build` are for building `wamrc` (next step). On Ubuntu
24.04 these pull LLVM 18.

### 2. Rust toolchain

```sh
rustup toolchain install nightly --component rust-src
rustup target add --toolchain nightly \
  riscv32imac-unknown-none-elf \
  riscv32imc-unknown-none-elf \
  wasm32-unknown-unknown
cargo install cargo-nextest --locked
cargo install espflash --locked   # optional: only for manual flash/monitor
```

The firmware build uses `-Zbuild-std=core,alloc`, which requires nightly +
`rust-src`. `riscv32imac` is for the C6, `riscv32imc` for the C3.

### 3. Build and install `wamrc` (the AOT compiler)

The AOT step shells out to `wamrc` 2.4.4; it must be on your `PATH`. This builds it
against the system LLVM 18 (no from-source LLVM build needed):

```sh
mkdir -p ~/wamr-build && cd ~/wamr-build
curl -fsSL \
  https://github.com/bytecodealliance/wasm-micro-runtime/archive/refs/tags/WAMR-2.4.4.tar.gz \
  | tar -xz
SRC=~/wamr-build/wasm-micro-runtime-WAMR-2.4.4
mkdir -p "$SRC/core/deps/llvm"
ln -sfn /usr/lib/llvm-18 "$SRC/core/deps/llvm/build"   # satisfies find_package(LLVM)
cmake -S "$SRC/wamr-compiler" -B build -G Ninja -DCMAKE_BUILD_TYPE=Release
cmake --build build
install -m 755 "$(readlink -f build/wamrc)" ~/.cargo/bin/wamrc   # ~/.cargo/bin is on PATH

wamrc --version   # -> wamrc 2.4.4
```

### 4. Serial access

The C6 enumerates as `/dev/ttyACM0`. Make sure you're in the `dialout` group:

```sh
sudo usermod -aG dialout "$USER"   # then log out/in for it to take effect
```

### 5. Build the firmware

The harness flashes a **pre-built** firmware ELF and **bails if it's missing** — it
does not build it for you. WiFi credentials are baked into the binary at **compile
time**, so build with them set:

```sh
cd embedded/esp-hal
WIFI_SSID='YourSSID' WIFI_PASS='YourPassword' cargo build-c6
```

Running from `embedded/esp-hal/` matters: its `rust-toolchain.toml` selects
nightly, and the `build-c6` alias + riscv link flags come from the workspace-root
cargo config. The ELF lands at (and is where the harness looks):

```
target/riscv32imac-unknown-none-elf/release/modem-esp32
```

> **Gotcha — changing WiFi creds:** `WIFI_SSID`/`WIFI_PASS` are read via
> `option_env!`, which is *not* tracked for rebuilds. To change them, force a
> rebuild first: `cargo clean -p esp-network` (or touch
> `embedded/esp-hal/crates/esp-network/src/session.rs`).

### 6. Sanity flash + monitor (optional but recommended)

Before involving the test harness, flash the firmware standalone and confirm the
board boots and joins your network:

```sh
espflash flash --monitor target/riscv32imac-unknown-none-elf/release/modem-esp32
```

Watch for: `Init!` → WiFi scan → `Wifi connected to …` → **`Got IP: <addr>`**.
You'll then see `Scouting for Zenoh nodes...` loop forever — that's expected with
no swarm running. Exit the monitor with **Ctrl-C** (this frees `/dev/ttyACM0` for
the tests).

---

## Running tests (setup already done)

The harness re-flashes the device from the pre-built ELF on every run, brings up a
local swarm, waits for the device to register, then builds/AOT-compiles/deploys the
cell(s). So:

- Rebuild the firmware (`cargo build-c6`) if you changed it — the harness uses
  whatever ELF is at `target/riscv32imac-unknown-none-elf/release/modem-esp32`.
- Make sure no `espflash` monitor (or anything else) is holding `/dev/ttyACM0`.

### Build the swarm binary

The harness spawns `swarm` as a child process; nextest won't build it for you:

```sh
cargo build --bin swarm
```

### Run

```sh
# whole suite
EMBEDDED_TARGET=ESP32C6 cargo nextest run -p hil-tests

# a single test (substring match on the test name)
EMBEDDED_TARGET=ESP32C6 cargo nextest run -p hil-tests sync_command_by_sorg_client

# ...or with an exact filterset
EMBEDDED_TARGET=ESP32C6 cargo nextest run -p hil-tests -E 'test(sync_command_by_sorg_client)'
```

Add **`--no-capture`** to stream the swarm logs and the device's serial output
live — essential when investigating a specific failure:

```sh
EMBEDDED_TARGET=ESP32C6 cargo nextest run -p hil-tests --no-capture <test_name>
```

### Relevant env vars

| Var | Default | Purpose |
|---|---|---|
| `EMBEDDED_TARGET` | _(unset → tests skip)_ | Enables the suite **and** selects the chip: `ESP32C5`, `ESP32C6`, `ESP32C61`. Drives the AOT target, the runtime tag, and the default firmware ELF path. |
| `ESPFLASH_PORT` | `/dev/ttyACM0` | Serial port for flashing + monitoring. |
| `EMBEDDED_ELF` | _(the ISA path below)_ | Override the firmware ELF the harness flashes. |

### How a test is structured

Every test is built on the shared `test-framework` scenario builder (`SwarmTest`), and follows the same three beats - 
the order matters, because the exec runtime under test *is* the device, so it cannot register until it has been flashed:

```rust
// 1. describe the scenario: this registers the cell classes in the swarm's datalayer
let spawned = hil_swarm_test()
    .aot_cell(assert_ok!(build_aot_cell("cell_atomic")), ATOMIC_CELL_SRI)
    .spawn()
    .await;

// 2. flash the device, so it boots, joins WiFi and registers as an exec runtime
let _monitor = assert_ok!(flash_device());

// 3. connect (waits for that registration), then let the orchestrator place the cells
let mut ctx = spawned.connect().await;
```

`hil_swarm_test()` (in `tests/integration/mod.rs`) pre-fills the swarm config, the runtime tag for `EMBEDDED_TARGET`, 
and the timeouts real hardware needs. Note the `_monitor` binding: dropping that handle stops the serial stream, so 
device output would vanish from the log.

### What a healthy run looks like

Roughly, in order (visible with `--no-capture`):

1. Local swarm + DB/filestore + orchestration/execution come up.
2. Harness flashes the board (espflash progress), serial log streams.
3. Device: `Got IP` → `Scouting...` → **`Using Zenoh node at <laptop-ip>:<port>`**.
4. `Registering exec runtime … name: "ESP32-C6", tags: [esp32c6, esp32, embedded]`.
5. Cell builds (`wasm-release`) → `wamrc` AOT (`Compile success … .aot`) → uploaded.
6. Device: `Received deploy command` → stored in flash → loaded into WAMR →
   instantiated → `Cell registered`.
7. The test's assertions run.

### Gotchas

- **Port busy / flash connect fails** — a leftover `espflash monitor` (or another
  process) is holding `/dev/ttyACM0`. Close it.
- **Device never finds the swarm** (`Scouting...` loops in a test run) — laptop and
  ESP aren't on the same subnet, or the network blocks multicast / isolates
  clients. Put both on the same WiFi, or rebuild the firmware with
  `TCP_DIRECT_ADDR=<laptop-ip>:<port>`.
- **`elf doesn't exist …`** — you haven't built the firmware (step 5), or you built
  a different target. The harness expects the C6 (`riscv32imac`) release ELF.
- **`"wamrc" binary not found`** — step 3 not done, or `~/.cargo/bin` not on `PATH`.
- **A `Tokio 1.x context … being shutdown` warning at teardown** is harmless.

## Signal-layer (dataplane) tests

The tests under `tests/integration/signal_layer/` exercise the **signal layer**
(the native drivers → processing-steps → tap-registry dataplane) end-to-end, on
hardware, read from a WASM cell. They live in the same suite but run against a
**different firmware image** than the cell tests: the plain cell-test ELF is
built *without* the dataplane, so it has no taps.

### The pipeline firmware

The dataplane build is the `modem-esp32` firmware with the `pipeline` feature,
generated from the **`hil-tests`** pipeline on the per-chip HIL board manifest
(`embedded/esp-hal/signal-layer/{pipelines/hil-tests.yaml,boards/esp32c6-hil.yaml}`;
use `esp32c5-hil` / `esp32c61-hil` for those chips). Build it — like the cell
firmware, the harness never builds it for you:

```sh
cd sdk/signal-layer
scripts/build.sh hil-tests --board esp32c6-hil
```

That regenerates `modem-esp32/src/pipeline_config.rs` (gitignored) and builds the
ELF at the usual `target/riscv32imac-unknown-none-elf/release/modem-esp32`.
Because that is the *same* path the cell-test ELF uses, keep the two straight:
either rebuild whichever firmware you're testing right before its run, or build
the pipeline ELF elsewhere and point the harness at it with **`EMBEDDED_ELF`**:

| Var | Default | Purpose |
|---|---|---|
| `EMBEDDED_ELF` | _(the C6 path above)_ | Override the firmware ELF the harness flashes — e.g. a saved pipeline build. |

### The rig

| Device | Wiring | Role |
|---|---|---|
| BME280 (0x76) | I2C0 — SCL=GPIO10, SDA=GPIO11 | real-value + moving-average tests |
| VEML7700 (0x10) | same I2C0 bus | real-value test |
| CCS811 (0x5A) | **declared but not populated** | health `Down` test (its absence *is* the test) |
| `sim` | synthetic (ignores the bus) | deterministic value / step / alarm tests |

The synthetic `sim` source (the `sim-source` driver) ramps `0,10,…,100,0,…` and
feeds a `threshold-trigger` at 50, so the value, moving-average, and alarm paths
assert exactly without depending on the environment. The real sensors use
range/liveness assertions.

### Running

```sh
# build the pipeline firmware first (above), then:
EMBEDDED_TARGET=ESP32C6 cargo nextest run -p hil-tests signal_layer

# a single dataplane test
EMBEDDED_TARGET=ESP32C6 cargo nextest run -p hil-tests sim_threshold_trigger_fires_at_the_threshold
```

Assertions go over Zenoh through the **`tap-bridge`** cell
(`tests/fixtures/tap-bridge/`), which the tests deploy automatically:
retained taps are read via its `read_tap` command, and the
`_signal_layer_health` and `sim_alarm` event taps are drained and republished
as the `tap_health` / `tap_alarm` Zenoh events via its `drain_events` command,
which the tests call on demand (the cell is command-driven — no periodic timer).
The `sim_source` / health / discovery tests need no sensors attached; the
`real_sensors` and `steps` tests need the BME280 (and VEML7700) wired.
