# First Steps on an ESP32

In this tutorial you take a blank ESP32-C6, flash it with Myrmic runtime firmware so it joins your swarm
as a node, and then deploy a *cell* onto it - the counter cell the CLI scaffolds - and drive it from
your keyboard. No sensors, no wiring: the shortest path from a bare board to your own code running on the 
microcontroller.

Along the way you learn what firmware is and how it differs from a cell, how a board joins the
swarm, how a cell is ahead-of-time compiled and placed onto a device, and how to send it commands
and read its logs.

## What you need

- The `myrmic` CLI and the Rust toolchain it builds cells with, per
  [Installation](../01_quickstart/01_installation.md).
- The embedded toolchain: the `riscv32imac-unknown-none-elf` Rust target (auto-installed on the
  first firmware build) and `wamrc` 2.4.4 on your `PATH`, the ahead-of-time compiler cells are
  compiled with for the device. See [Installation (Embedded)](../01_quickstart/02_installation-embedded.md)
  for that one-time setup - this tutorial needs nothing beyond it.
- An ESP32-C6 devkit and a machine to flash it from.
- WiFi credentials for the network that machine is on. The board discovers the swarm by multicast;
  a fallback for networks that block it is in Step 2.

Nothing here needs a clone of the Myrmic repository: the scaffolded cell and firmware pin their dependencies to git 
revisions and fetch them on the first build.

You will use two terminals:

| Terminal | Runs |
|---|---|
| Terminal 1 | your working shell: start the runtime, scaffold, deploy, send |
| Terminal 2 | `myrmic flash --monitor` - the board's live serial output |

---

## Step 1 - Start a runtime

The CLI talks to the swarm through a *runtime*, a Myrmic process. On one machine that single runtime
*is* your swarm, and it is what the board will join. In Terminal 1, start one in the background with a
throwaway in-memory database:

```bash
myrmic runtimes start -d -n dev --tmp
```

Expected output:

```text
INFO  runtime "dev" pid file: /run/user/1000/myrmic/dev.pid
INFO  Starting runtime "dev"(ec1bcf44056d4d338e254167b8c20ba9)
```

Leave it running. Everything else in Terminal 1 - `deploy`, `network`, `send` - reaches the swarm
through it.

---

## Step 2 - Scaffold and flash the firmware

*Firmware* is a native RISC-V binary: a node that runs on the bare-metal chip and joins the swarm.
Unlike a cell it is not WebAssembly, and it is *flashed*, never deployed. `myrmic new --firmware`
scaffolds a firmware crate; `=esp32c6` selects the chip. On Terminal 2:

```bash
myrmic new --firmware=esp32c6 ~/esp-tutorial/my-node
```

Expected output:

```text
INFO  Creating firmware 'my-node' for esp32c6
```

Now build and flash it. The WiFi credentials are baked into the firmware at compile time, so set
them in the environment first, then flash in **Terminal 2** with `--monitor` to stream the serial
output afterwards:

```bash
cd ~/esp-tutorial/my-node
export WIFI_SSID="your-network" WIFI_PASS="your-password"
myrmic flash --monitor
```

The first build is the slow one: it auto-installs the embedded toolchain, fetches the ESP SDK
crates, and compiles `core`, `alloc` and the WAMR runtime from source. `myrmic flash` then connects
to the board, writes the image sized to the device's flash, and resets into it:

```text
Chip type:         esp32c6 (revision v0.2)
Features:          WiFi 6, BT 5
MAC address:       10:51:db:01:fa:3c
INFO  Building esp32c6 firmware: .../my-node/Cargo.toml
    Finished `release` profile [optimized] target(s) in 41s
INFO  Flashing .../my-node...
App/part. size:    2,274,192/3,080,192 bytes, 73.83%
INFO  Flashed; the board is booting
```

The board finds the runtime the same way the CLI does: by scouting the local network. On an ordinary
flat network (board and computer on one router) that simply works. 

Expected in the serial output, in this order:

```text
INFO - Wifi connected to ConnectedInfo { ssid: "your-network", ... }
INFO - Got IP: 192.168.1.60
INFO - Scouting for Zenoh nodes...
INFO - Using Zenoh node at 192.168.1.68:46747
INFO - clock synced to swarm time (1789666899s since epoch)
INFO - Registering exec runtime with info: ExecRuntimeInfo { id: RuntimeId(796d3cfa01db5110), name: Some("ESP32-C6"), capabilities: ExecutionCapabilities { tags: [CapabilityTag("esp32c6"), CapabilityTag("esp32"), CapabilityTag("riscv32imac"), CapabilityTag("embedded"), CapabilityTag("wifi-myrmic"), CapabilityTag("gpio"), ...] } }
```

>If your network blocks multicast, give the firmware the runtime's address at build time instead by setting 
`TCP_DIRECT_ADDR` to the computer's address and the runtime's listen port before flashing.
>```bash
>  export WIFI_SSID="your-network" WIFI_PASS="your-password" TCP_DIRECT_ADDR="192.168.1.68:46747"
>  myrmic flash --monitor
>```

Read those last three lines as the board joining: it found the runtime your computer anchors, synced
to swarm time, and registered itself as a place to run cells. Leave the monitor streaming in
Terminal 2.

---

## Step 3 - See the board in the swarm

Back in Terminal 1, list the swarm nodes:

```bash
myrmic network
```

Expected output - two nodes now, the board and your `dev` runtime:

```text
Discovered 2 node(s)

  #  name      kind     id          tags
  0  ESP32-C6  esp32c6  [7]96d3cfa  esp32c6, esp32, riscv32imac, embedded, wifi-myrmic, gpio, @796d3cfa01db5110
  1  dev       linux    [e]c1bcf44  @ec1bcf44056d4d338e254167b8c20ba9, linux
```

The board joined as `ESP32-C6`, carrying platform *tags* - facts about what the node is, not
preferences: `esp32c6`, `riscv32imac`, `embedded`, and so on. The `esp32c6` tag is how you will pin a
cell to the board in Step 5. The same tags show under:

```bash
myrmic tags
```

```text
NODE      ID          TAGS
ESP32-C6  [7]96d3cfa  esp32c6, esp32, riscv32imac, embedded, wifi-myrmic, gpio, @796d3cfa01db5110
dev       [e]c1bcf44  @ec1bcf44056d4d338e254167b8c20ba9, linux
```

---

## Step 4 - Scaffold the cell

A *cell* is a Rust crate compiled to WebAssembly and run on a runtime. Scaffold the canonical one -
the counter:

```bash
myrmic new ~/esp-tutorial/counter
```

Expected output:

```text
INFO  Creating 'counter'
```

You do not write a line of Rust in this tutorial. The scaffold already defines commands over a
durable count kept in the cell's private state - `increment`, `decrement`, and `count`, which logs
the current value:

```rust
#[myrmic_sdk::cmd]
fn increment(md: Metadata) -> myrmic_sdk::Result {
    let count = STATE.upsert_with(|count| {
        *count = *count + 1;
    })?;

    let _ = myrmic_sdk::info!("Incremented count to {} (sender={:?})", count, md.sender).ok();

    Ok(())
}
```

That is the same cell that runs on a Linux runtime. Nothing about it is embedded-specific; the target
is chosen at deploy time, not written into the code.

---

## Step 5 - Deploy the cell onto the board

Deploy the cell for the device platform `riscv32imac`. That rebuilds it and ahead-of-time compiles
it with `wamrc`. Your Linux runtime is in the swarm too and could host the cell, so `--tag esp32c6`
pins the placement to the board.

One flag matters here: `--timeout`. A cell's first deployment to a board transfers the compiled
module over the network and writes it into the device's flash, which takes longer than the CLI's
default wait. Give it room - a redeploy of the *same* module skips the transfer and returns at once,
so the longer timeout only ever costs you on the first deploy or a slow network:

```bash
myrmic deploy ~/esp-tutorial/counter --platform riscv32imac --tag esp32c6 --timeout 90s
```

Expected output - `wamrc` compiles the module, then the runtime confirms placement on the board:

```text
Compile success, file .../counter/target/counter.aot was generated.
INFO  deploying cell (srn = counter, sri = 5d883103-6382-5d16-9a0c-d70fd0565cb6)
INFO  deployed cell (srn = counter, sri = 5d883103-6382-5d16-9a0c-d70fd0565cb6, runtime = 796d3cfa01db5110)
```

The serial monitor in Terminal 2 shows the board's side - it receives the module, loads it into its
WAMR runtime, and registers the cell:

```text
INFO - [db] Requesting WASM metadata...
INFO - WASM module found. Loading into WAMR...
INFO - WAMR engine initialized
INFO - WASM Module instantiated
INFO - Registering the Cell
```

See where the cell landed:

```bash
myrmic cells status
```

```text
  cell     sri                                   kind  runtime     age  policy  class    srn
──── counter ─────────────────────────────────────────────────────────────────────────
  counter  5d883103-6382-5d16-9a0c-d70fd0565cb6  wasm  [7]96d3cfa  9s   never   counter  counter
```

The `runtime` column is the proof: `[7]96d3cfa` is the board's id, not `dev`. The cell is running on
the microcontroller.

---

## Step 6 - Drive the cell

Send it a command. `increment` takes no reply, so the CLI just prints a trace id and confirms the
runtime accepted it:

```bash
myrmic send counter increment
myrmic send counter increment
```

```text
INFO  trace ID = 5675f09fddf7e9aa19ac2efae3f3a296
INFO  successfully sent command
```

Watch the serial monitor in Terminal 2 react to each one. The sender is nil because the command came
from the CLI, not another cell:

```text
INFO - module log: Incremented count to 1 (sender=Sri(00000000-0000-0000-0000-000000000000))
INFO - module log: Incremented count to 2 (sender=Sri(00000000-0000-0000-0000-000000000000))
```

Ask the cell for its count. Sent from the CLI there is no caller to answer, so it logs the value
rather than returning it:

```bash
myrmic send counter count
```

```text
INFO - module log: Count is 2 (no caller to answer)
```

The count is state the cell persisted on the board: send `increment` again, or `decrement`, and the
value moves from where it left off. You have deployed your own cell to an ESP32 and driven it from
your keyboard, without wiring a single pin.

---

## What Have You Learned

- *Firmware* is a native binary flashed to the chip; a *cell* is WebAssembly deployed onto a
  runtime. The board runs firmware and hosts cells.
- The CLI reaches the swarm through a runtime, and a flashed board joins that swarm over WiFi as a
  node carrying an `esp32c6` tag.
- A cell is written once and targeted at deploy time: `--platform riscv32imac` ahead-of-time
  compiles it with `wamrc`, and `--tag esp32c6` places it on the board rather than the Linux runtime.
- A first deployment to a board transfers the module into flash, so it needs a longer `--timeout`;
  redeploying the same module skips the transfer.
- `myrmic send` drives a deployed cell, and `myrmic flash --monitor` shows the board's own logs as it
  handles each command.

## Next Step

- [First Steps with the Signal Layer](./04_first-steps-signal-layer.md) - give the board sensors and
  actuators, and read them from a cell over named taps and outlets.
- [`myrmic deploy`](../10_reference/02_myrmic-cli/05_deploy.md) and
  [`myrmic new`](../10_reference/02_myrmic-cli/01_new.md) - the full command reference.
