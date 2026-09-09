# esp-firmware

The myrmic ESP32 firmware, as a library. Your crate owns `main`; this brings up
the node — WiFi, the zenoh session, the db client, the hardware watchdogs, and a
host for a WASM cell — and gets out of the way where you want to do it yourself.

## The rule

**Ownership decides what runs.** [`Board`] holds the peripherals the shipped
subsystems need. `start` brings up only what is still on the board when it runs,
so taking a peripheral is at once "don't run yours" and "give me the hardware",
and the borrow checker guarantees we are not still driving something you took.

| Take | And the firmware stops |
| ---- | ---------------------- |
| `take_ble()` | the NimBLE host and its HCI transport |
| `take_wifi()` | the network service (WiFi, zenoh, db) |
| `take_wasm_host()` | the WAMR thread and module storage |
| `take_watchdog()` | arming the MWDT/RWDT and the required-task heartbeat |
| `take_pin(n)` | offering that GPIO to the cell |

There are no `enable_*` flags and no subsystem traits. `Config` carries only the
things that are numbers — thread stack sizes and priorities.

## A firmware

```rust
#![no_std]
#![no_main]

#[esp_firmware::main]
async fn setup() {}
```

That is the whole modem firmware. To change something, ask for what you need —
any of `&mut Board`, `Network`, `Spawner` and `Peripherals`, in any order:

```rust
#[esp_firmware::main]
async fn setup(board: &mut Board, spawner: Spawner) {
    // Our BLE stack, not the shipped one.
    if let Some(bt) = board.take_ble() {
        spawner.spawn(my_ble_stack(bt).unwrap());
    }

    // The onboard LED is ours; the cell can no longer drive GPIO8.
    if let Some(led) = board.take_pin(8) {
        spawner.spawn(blink(led).unwrap());
    }
}
```

Everything still on the board when `setup` returns is started for you.

## Being the cell yourself

A node hosts one cell. Normally the orchestrator fills that slot with a WASM
module; `Network::register` fills it from the firmware, and then no WASM runtime
is built at all — no WAMR thread, no module storage, no AOT flash region.

```rust
#[esp_firmware::main]
async fn setup(board: &mut Board, net: Network, spawner: Spawner) {
    let led = board.take_pin(8).unwrap();
    let cell = net.register("demo/thermostat", &["set_target", "read"]).unwrap();
    spawner.spawn(run(cell, led).unwrap());
}
```

`register` does not block. An SRI is a UUIDv5 folded over the segments of an
SRN, so the identity is resolved offline, before the radio is up — which is why
the decision not to build a WASM host never depends on the network coming up.
Any other cell addressing `demo/thermostat` derives the same SRI.

Being *reachable* does need the network: the placement row that tells the swarm
this node holds the cell is written by the db service on its own reconcile tick,
retried on failure and re-asserted after a reconnect, exactly as this node's
exec registration and lease renewal already are. `Cell::online()` waits for it
if you care; `Cell::recv()` simply stays quiet until then.

That row also marks the node **occupied**, so the orchestrator will not place a
WASM cell here — and if one is somehow directed at this node anyway, the deploy
is refused rather than displacing your firmware's cell.

## Taps and outlets

The two directions across the cell boundary. A `Tap` is a value the firmware
publishes and the cell reads; an `Outlet` is a command the cell writes and the
firmware acts on. A Signal Layer pipeline declares them from YAML; a firmware
can declare its own on the board, and the cell cannot tell the two apart.

```rust
#[esp_firmware::main]
async fn setup(board: &mut Board, spawner: Spawner) {
    let temperature = board.tap::<f32>("temperature").unwrap();
    let relay = board.outlet::<DigitalState>("relay").unwrap();
    let pin = board.take_pin(8).unwrap();
    spawner.spawn(sample(temperature).unwrap());
    spawner.spawn(drive(relay, pin).unwrap());
}

#[embassy_executor::task]
async fn drive(relay: Outlet<DigitalState>, mut pin: Flex<'static>) {
    loop {
        let (_at, cmd) = relay.changed().await;   // wakes on the cell's write
        pin.set_level(cmd.on.into());
    }
}
```

`Tap::update` stamps the value with the time of the call; `EventTap::emit`
queues a discrete event; `Outlet::changed` waits for the next command and
`Outlet::read` peeks at the latest. The handles are `Copy` and `'static`, so
they move into tasks freely. Names are the contract with the cell, which
resolves them through `myrmic_sdk::tap` and `myrmic_sdk::outlet`; payloads
cross as postcard and the cell checks their type id, so use a primitive or one
of the `signal_layer_types` both sides know. `start` hands the registries to the runtime, so declare before it
runs — in `setup`, or before your own `start` call.

A generated pipeline registers its slots into the same registries through
`board.taps()` and `board.outlets()`, so pipeline and hand-declared slots
coexist on one board.

## Peripherals the board does not hold

`Board` holds only what the shipped subsystems use. RMT, SPI, I2C, LEDC and the
rest stay in `Peripherals`, which you can ask for next to `&mut Board`: it is
whatever the boot sequence and `board!` did not claim, and moving a field out
of it is the whole gesture. A field the board did take is a "use of moved
value" error at compile time, not a conflict at runtime.

```rust
#[esp_firmware::main]
async fn setup(board: &mut Board, peripherals: Peripherals, spawner: Spawner) {
    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(80)).unwrap();
    let led = board.take_pin(8).unwrap();
    let channel = rmt.channel0.configure_tx(&config).unwrap().with_pin(led);
    spawner.spawn(drive_ws2812(channel).unwrap());
}
```

## Taking over the hardware claim

Ask for `Peripherals` without a `Board` and nothing is claimed for you: the
boot sequence still runs, but building the board and calling `start` are yours.
This is the form for a board that must claim its own hardware before `board!`
decides what the cell may have — a Signal Layer pipeline, say:

```rust
#[esp_firmware::main]
async fn setup(peripherals: Peripherals, spawner: Spawner) {
    let pins = pipeline_pins!(peripherals);
    let board = esp_firmware::board!(peripherals, pins = pins);
    esp_firmware::start(board, spawner);
}
```

`board!` is a macro, not a function, because claiming moves individual fields
out of `Peripherals` — which only works inline, on a binding with a plain name.
Everything it does not name stays yours.

## What your crate owns

- `#![no_std]` and `#![no_main]` — an attribute macro cannot add crate-level
  attributes.
- A `build.rs` of one line: `esp_firmware_build::configure();`. It turns an
  optional `partitions.toml` into the AOT flash layout, the app partition table
  `espflash` consumes, and a link-time floor under the main stack.
- Chip features (`esp32c5`, `esp32c6`, `esp32c61`) that forward to this crate.
  These cannot be inherited: the GPIO map is selected by the feature set of the
  crate that invokes `board!`, and `esp_firmware_build::configure()` reads the
  chip from the same feature set.
- One dependency. `#[esp_firmware::main]` reaches `esp-hal`, `esp-rtos` and
  `embassy-executor` only through this crate's re-exports, so a firmware that
  names none of them itself depends on `esp-firmware` alone. Use
  `esp_firmware::{esp_hal, esp_rtos, embassy_executor}` in your own code too
  and you cannot end up with two versions; `#[embassy_executor::task]` is the
  one thing that still wants `embassy-executor` as a direct dependency.
- `.cargo/config.toml` with the riscv target rustflags, and (for BLE) a
  `nimble-config.toml` at your workspace root.

[`../../firmware-examples/`](../../firmware-examples/) is a working crate with
all of the above; copy it to start a new firmware.

## Where things live

| Path | Responsibility |
| ---- | -------------- |
| [`src/lib.rs`](src/lib.rs) | `start` — the boot order, priorities and the channels wiring the subsystems together. |
| [`src/board.rs`](src/board.rs) | `Board` and the `board!` claim macro. |
| [`src/cell.rs`](src/cell.rs) | `Network`, `Cell` — native cell registration and the myrmic message API. |
| [`src/tap.rs`](src/tap.rs) | `Tap`, `EventTap` — values the firmware publishes for the cell. |
| [`src/outlet.rs`](src/outlet.rs) | `Outlet` — commands the cell writes for the firmware, with a wake on write. |
| [`src/config.rs`](src/config.rs) | Stack sizes and thread priorities, with the reasoning for each default. |
| [`src/net.rs`](src/net.rs) | The network-service thread: WiFi, zenoh session, cell db service. |
| [`src/wasm.rs`](src/wasm.rs) | The WASM host tasks (request handler, cell pump, module storage). |
| [`src/ble.rs`](src/ble.rs) | The shipped NimBLE stack and its HCI transport thread. |

The subsystem bodies live in [`../`](../) — `esp-network`, `cell-db-service`,
`esp-watchdog`, `wasm-runtime`. This crate is the assembly, and the tasks stay
here so the embassy-executor version remains a firmware-side choice.
