# cell-tools Tutorial

This tutorial walks through the full cell authoring workflow: writing cells,
building them with `cell-tools`, deploying them to a running swarm, and
verifying their interaction with `cell-ctl`.

You will build three cells:
- A **room** cell that stores a `Temperature` domain type and publishes a
  `temperature_changed` event whenever the temperature is set
- A **heater** cell that imports the room API and bumps the room temperature
  up by one degree on each call
- A **thermostat** cell that imports both the room and heater APIs, starts a
  heating loop by calling the heater, and reacts to temperature change events
  to keep calling the heater until a target temperature is reached

The tutorial demonstrates:
- Synchronous queries for on-demand inspection (`get_temperature` via `cell-ctl`)
- Events as domain-level notifications (room announces temperature changes)
- Event-driven control loops (thermostat reacts to changes, not polling)
- State shared across command and event handler invocations on the same cell
- The `#[event]` attribute, `#[event_handler]`, and `import_cells!` macro

All commands assume you are running from the `swarm/tutorials/cell-tools/`
directory.

## Prerequisites

If you don't have Rust installed, get it via [rustup](https://rustup.rs/):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Cell code compiles to WebAssembly using the Rust nightly toolchain. Install it
along with the required target and component:

```sh
rustup toolchain install nightly
rustup target add wasm32-unknown-unknown --toolchain nightly
rustup component add rust-src --toolchain nightly
```

## Step 1 -- Setup

Run the setup script to build all required binaries:

```sh
./setup.sh
```

This builds `swarm-cli`, `cell-ctl`, and `cell-tools`, then
copies the binaries into the `workspace/` directory:

```
workspace/swarm       -- the swarm runtime
workspace/cell-ctl    -- deploys cells and sends commands to the swarm
workspace/cell-tools  -- builds cell crates into .wasm binaries and API files
```

## Step 2 -- Create the room cell workspace

Create a new workspace for the room cell:

```sh
./new-workspace.sh room
```

This copies the starter template into `workspace/room/` and sets up a Cargo
workspace with the correct dependencies. The workspace structure looks like
this:

```
workspace/room/
  Cargo.toml              -- workspace root with myrmic-sdk dependencies
  room/
    Cargo.toml            -- the room logic crate
    src/
      lib.rs              -- starter file (you will edit this next)
```

## Step 3 -- Write the room cell logic

Open `workspace/room/room/src/lib.rs` and replace its contents with:

```rust
#![no_std]

extern crate alloc;
use alloc::format;
use alloc::string::String;

use serde::{Deserialize, Serialize};
use myrmic_sdk::info;
use myrmic_sdk_macros::{cell, event};

/// A temperature reading in degrees Celsius.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Clone, Copy)]
pub struct Temperature {
    pub degrees_celsius: i32,
}

/// Published when the room temperature changes.
#[derive(Serialize)]
#[event]
pub struct TemperatureChanged {
    pub temperature: Temperature,
}

#[derive(Default, Serialize, Deserialize)]
struct Room {
    temperature: Temperature,
}

#[cell]
impl Room {
    #[init]
    fn init() -> Self {
        Room {
            temperature: Temperature {
                degrees_celsius: 20,
            },
        }
    }

    /// Returns the current temperature as a human-readable string.
    #[command]
    fn get_temperature(&self) -> String {
        format!("{} degrees", self.temperature.degrees_celsius)
    }

    /// Returns the current temperature as a structured value.
    #[command]
    fn read_temperature(&self) -> Temperature {
        self.temperature
    }

    /// Sets the room temperature and publishes a change event.
    #[command]
    fn set_temperature(&mut self, temperature: Temperature) {
        self.temperature = temperature;
        info!(
            "[ROOM]: my temperature changed to {t}; Publishing new temp value",
            t = self.temperature.degrees_celsius
        )
        .expect("logging should work");
        let _ = self.publish_event(TemperatureChanged {
            temperature: self.temperature,
        });
    }
}

```

This defines a `Room` cell with an initial temperature of 20 degrees. Key
things to note:

- `Temperature` is a domain type. It will appear in the generated API file so
  that other cells can use it through `import_cells!`.
- `TemperatureChanged` is an event struct marked with `#[event]`. The macro
  generates a `CellEvent` trait implementation that maps the struct name to the
  event topic `temperature_changed` (snake_case conversion).
- `get_temperature` returns a `String` so that `cell-ctl` can display the
  result directly. `read_temperature` returns the structured `Temperature`
  type for programmatic use by other cells.
- `set_temperature` updates the state and publishes a `TemperatureChanged`
  event using `self.publish_event(...)`. This method is generated by the
  `#[cell]` macro on any cell that defines event structs.

## Step 4 -- Build the room cell

Build the room cell using `cell-tools`:

```sh
./workspace/cell-tools build ./workspace/room/room room
```

This produces two artifacts inside `workspace/room/target/cells/`:

```
workspace/room/target/cells/room.wasm       -- the deployable Wasm binary
workspace/room/target/cells/room-api.yml    -- the API file describing commands, types, and events
```

The `.wasm` file is what gets deployed to the swarm. The API file is what other
cells import (via the `import_cells!` macro) to generate client code, domain
types, and event payload types at compile time.

Verify the build succeeded by checking that both files exist:

```sh
ls workspace/room/target/cells/
```

You should see `room-api.yml` and `room.wasm`.

## Step 5 -- Create the heater cell workspace

Create a second workspace for the heater cell:

```sh
./new-workspace.sh heater
```

This creates `workspace/heater/` with the same starter template structure.

## Step 6 -- Write the heater cell logic

The heater cell imports the room cell's API to get the `RoomClient` struct and
the `Temperature` type. It does not need a `room-client` dependency in
`Cargo.toml` -- the `import_cells!` macro generates everything inline from the
API file.

Open `workspace/heater/heater/src/lib.rs` and replace its contents with:

```rust
#![no_std]

use serde::{Deserialize, Serialize};
use myrmic_sdk::{Result, info};
use myrmic_sdk_macros::{cell, import_cells};

extern crate alloc;
use alloc::string::String;

import_cells!("../../room/target/cells/room-api.yml");

const ROOM_CELL_SRI: &str = "room_cell";

#[derive(Default, Serialize, Deserialize)]
struct Heater {}

#[cell]
impl Heater {
    #[init]
    fn init() -> Self {
        Heater {}
    }

    /// Reads the room temperature, adds one degree, and sets it back.
    #[command]
    fn heat_room() -> Result<()> {
        let room = RoomClient::new(ROOM_CELL_SRI);
        let current = room.read_temperature()?;
        info!("[HEATER]: heating up the temperature by 1 degree").unwrap();
        room.set_temperature(Temperature {
            degrees_celsius: current.degrees_celsius + 1,
        })?;
        Ok(())
    }
}
```

Key points:

- `import_cells!` takes a path to the room's API file, relative to the heater
  crate's root directory (the directory containing `Cargo.toml`). The path
  `"../../room/target/cells/room-api.yml"` traverses up from
  `workspace/heater/heater/` to `workspace/` and then into the room's output.
- The macro generates `RoomClient` (with methods for each of the room's
  commands) and the `Temperature` struct. You use them directly in your code
  without any additional imports or dependencies.
- `ROOM_CELL_SRI` is the resource identifier that will be used when deploying
  the room cell. The heater uses this to address commands to the right cell.
- The heater is stateless -- it has no fields. Each `heat_room` call reads the
  current room temperature via `read_temperature`, increments it by one degree,
  and writes it back via `set_temperature`. That write triggers the room to
  publish a `temperature_changed` event.

## Step 7 -- Build the heater cell

Build the heater cell:

```sh
./workspace/cell-tools build ./workspace/heater/heater heater
```

Verify the build succeeded:

```sh
ls workspace/heater/target/cells/
```

You should see `heater-api.yml` and `heater.wasm`.

## Step 8 -- Create the thermostat cell workspace

Create a third workspace for the thermostat cell:

```sh
./new-workspace.sh thermostat
```

## Step 9 -- Write the thermostat cell logic

The thermostat imports both the room and heater APIs. It provides a
string-based `heat_to` command that starts a heating loop, and an event handler
that reacts to temperature changes to continue heating until the target is
reached.

Open `workspace/thermostat/thermostat/src/lib.rs` and replace its contents
with:

```rust
#![no_std]

extern crate alloc;
use alloc::format;
use alloc::string::String;

use serde::{Deserialize, Serialize};
use myrmic_sdk::{Result, info};
use myrmic_sdk_macros::{cell, import_cells};

import_cells!(
    "../../room/target/cells/room-api.yml",
    "../../heater/target/cells/heater-api.yml",
);

const HEATER_SRI: &str = "heater_cell";

#[derive(Serialize, Deserialize)]
struct Thermostat {
    target: Temperature,
}

#[cell]
impl Thermostat {
    #[init]
    fn init() -> Self {
        Thermostat {
            target: Temperature { degrees_celsius: 0 },
        }
    }

    /// Parses a target temperature from the payload string, stores it,
    /// kicks off the first heating step, and returns an acknowledgment.
    #[command]
    fn heat_to(&mut self, payload: String) -> Result<String> {
        info!("[THERMO]: user wants the room to be at {payload}").unwrap();

        let degrees: i32 = payload
            .trim()
            .parse()
            .map_err(|_| "failed to parse temperature from string")?;
        self.target = Temperature {
            degrees_celsius: degrees,
        };
        let heater = HeaterClient::new(HEATER_SRI);
        heater.heat_room()?;
        Ok(format!("heating to {}", degrees))
    }

    /// Reacts to room temperature changes. If the new temperature is still
    /// below the target, calls the heater again to continue heating.
    #[event_handler]
    fn temperature_changed(&mut self, event: TemperatureChanged) {
        info!("[THERMO]: Was informed about current room temperature").unwrap();
        if event.temperature.degrees_celsius < self.target.degrees_celsius {
            info!("[THERMO]: Room still too cold -> Activating heater").unwrap();
            let heater = HeaterClient::new(HEATER_SRI);
            let _ = heater.heat_room();
        }else{
            info!("[THERMO]: Room now at desired temperature").unwrap();
        }
    }
}

```

Key points:

- `import_cells!` accepts multiple API file paths, separated by commas. The
  `Temperature` type appears in both the room and heater APIs, but the macro
  deduplicates it -- only one `Temperature` struct is generated. The room's
  `TemperatureChanged` event type is also generated from the `events` section
  of the room API file.
- The thermostat stores a `target` temperature in its state. State is
  persisted across command and event handler invocations, so the event handler
  can read the target that was set by the `heat_to` command.
- `heat_to` is a string-based command: it receives a `String` payload from
  `cell-ctl`, parses it into a number, stores the target, calls
  `heater.heat_room()` to start the first heating step, and returns an
  acknowledgment string.
- `#[event_handler]` marks `temperature_changed` as an event handler. The
  method name must match the event topic name (`temperature_changed`). The
  `#[cell]` macro automatically subscribes to this event during cell
  initialization.
- When the room publishes a `temperature_changed` event (because the heater
  called `set_temperature`), the thermostat's handler fires. If the new
  temperature is still below target, it calls the heater again, which bumps
  the room by another degree, which publishes another event, and so on --
  forming an event-driven control loop that converges on the target.

## Step 10 -- Build the thermostat cell

Build the thermostat cell:

```sh
./workspace/cell-tools build ./workspace/thermostat/thermostat thermostat
```

Verify the build succeeded:

```sh
ls workspace/thermostat/target/cells/
```

You should see `thermostat-api.yml` and `thermostat.wasm`.

## Step 11 -- Start the swarm

Start the swarm runtime in the background (or better, in a different terminal). The tutorial includes a
`swarm-config.jsonnet` that configures a single-node swarm (which is what runs your cell logic).

```sh
RUST_LOG=sorg_execution=info ./workspace/swarm ./swarm-config.jsonnet &
SWARM_PID=$!
```

Wait a few seconds for the swarm to finish starting up:

```sh
sleep 3
```

If you are following this tutorial interactively, you may prefer to start the
swarm in a separate terminal instead of backgrounding it. In that case, run
the same command without the trailing `&` and `SWARM_PID` line, then continue
in your original terminal.

Verify the swarm is running by checking cell status:

```sh
./workspace/cell-ctl status
```

You should see on cells in the output at this point.

## Step 12 -- Deploy all three cells

Deploy the room cell first. The SRI (swarm resource identifier) must be
`room_cell` -- this is the value the heater cell has hardcoded as its target.

```sh
./workspace/cell-ctl deploy room_cell --wasm ./workspace/room/target/cells/room.wasm
```

Deploy the heater cell. Its SRI must be `heater_cell` -- the thermostat has
this hardcoded.

```sh
./workspace/cell-ctl deploy heater_cell --wasm ./workspace/heater/target/cells/heater.wasm
```

Deploy the thermostat cell:

```sh
./workspace/cell-ctl deploy my_thermostat --wasm ./workspace/thermostat/target/cells/thermostat.wasm
```

Verify all cells are registered:

```sh
./workspace/cell-ctl status
```

You should see the `room_cell`, `heater_cell`, and
`my_thermostat` in the output.

## Step 13 -- Send commands and verify interaction

### Query the room temperature directly

```sh
./workspace/cell-ctl command room_cell --name get_temperature
```

Since `get_temperature` returns a `String`, `cell-ctl` displays the result
directly. You should see `20 degrees` in the response.

### Start the heating loop

Send the `heat_to` command to the thermostat with a target of 25 degrees:

```sh
./workspace/cell-ctl command my_thermostat --name heat_to --payload "25"
```

The thermostat parses the string `"25"`, stores it as the target, calls
`heater.heat_room()` to start the first step, and returns an acknowledgment.
You should see `heating to 25` in the response.

Behind the scenes, the following loop runs automatically:

1. The heater reads the room temperature (20), sets it to 21
2. The room publishes a `temperature_changed` event with temperature 21
3. The thermostat's event handler fires, sees 21 < 25, calls `heater.heat_room()`
4. The heater reads 21, sets it to 22
5. The room publishes an event with 22
6. ... this continues until the room reaches 25
7. The thermostat's event handler sees 25 >= 25 and stops

This entire loop happens within the swarm -- the `heat_to` command returns
immediately after the first heating step, and the rest is event-driven.

### Verify the final temperature

Query the room temperature again to confirm the heating loop reached the
target:

```sh
./workspace/cell-ctl command room_cell --name get_temperature
```

You should see `25 degrees` in the response.

## Step 14 -- Cleanup

When you are done, run the cleanup script:

```sh
./cleanup.sh
```

This stops the background swarm process and removes all generated files from
the workspace.

If you started the swarm in a separate terminal, stop it with Ctrl-C before
running cleanup.
