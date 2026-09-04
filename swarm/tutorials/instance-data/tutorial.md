# Instance Data Tutorial

This tutorial demonstrates how to deploy multiple cell instances from the
same cell class, each initialized with different state using **instance
data**.

You will build two cell classes:

- A **room** cell that stores a temperature value. Each room instance
  starts with a different initial temperature, set via instance data.
- A **controller** cell that reads the temperature of a specific room.
  Which room it talks to is configured per instance via instance data.

You will deploy four instances total:

| Instance         | Class      | Instance data                    | Deployed via   |
|------------------|------------|----------------------------------|----------------|
| `room.001`       | room       | `{"degrees_celsius": 20}`        | app-spec       |
| `room.002`       | room       | `{"degrees_celsius": 18}`        | app-spec       |
| `controller.001` | controller | `{"room_sri": "room.001"}`       | app-spec       |
| `controller.002` | controller | `{"room_sri": "room.002"}`       | CLI flag       |

The tutorial covers both ways to provide instance data:

- **`instance_data`** -- inline JSON in the app-spec YAML
- **`instance_data_file`** -- a path to an external JSON file
- **`--instance-data`** -- a CLI flag for single-cell deploys

All commands assume you are running from the
`swarm/tutorials/instance-data/` directory.

## Prerequisites

If you don't have Rust installed, get it via [rustup](https://rustup.rs/):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Cell code compiles to WebAssembly using the Rust nightly toolchain. Install
it along with the required target and component:

```sh
rustup toolchain install nightly
rustup target add wasm32-unknown-unknown --toolchain nightly
rustup component add rust-src --toolchain nightly
```

## Step 1 -- Setup

Run the setup script to build the myrmic CLI:

```sh
./setup.sh
```

This produces:

```
workspace/myrmic   -- the myrmic CLI (scaffold, build, deploy, manage, runtime)
```

Next, point the SDK at your local checkout so that `myrmic new` uses it
instead of fetching over the network:

```sh
export PEERIOT_WASM_SDK="$(cd ../../../sdk/myrmic-sdk && pwd)"
```

This environment variable must stay set for the rest of the tutorial.

## Step 2 -- Create the room cell

Scaffold a new cell project for the room:

```sh
./workspace/myrmic new ./workspace/room
```

This creates a starter crate in `workspace/room/` with a `Cargo.toml` and
`src/lib.rs`.

## Step 3 -- Write the room cell logic

Open `workspace/room/src/lib.rs` and replace its contents with:

```rust
#![no_std]

use serde::{Deserialize, Serialize};
use myrmic_sdk::{String, format, macros::cell};

#[derive(Default, Serialize, Deserialize)]
struct Room {
    degrees_celsius: i32,
}

#[cell(state_serde = "json")]
impl Room {
    #[init]
    #[must_use]
    fn init() -> Self {
        Room { degrees_celsius: 0 }
    }

    #[command]
    fn get_temperature(&self) -> String {
        format!("{} degrees", self.degrees_celsius)
    }
}
```

Key points:

- `#[cell(state_serde = "json")]` tells the cell macro to deserialize the
  cell's state from JSON. When instance data is provided at deploy time,
  the JSON bytes are deserialized into the `Room` struct, bypassing the
  `#[init]` constructor entirely. Without instance data, `init` runs as
  usual.
- The `Room` struct is the cell's state. Its fields map directly to the
  JSON keys in the instance data -- `{"degrees_celsius": 20}` becomes
  `Room { degrees_celsius: 20 }`.
- `get_temperature` returns a human-readable `String` so that `myrmic send`
  can display the result directly.

## Step 4 -- Build the room cell

```sh
./workspace/myrmic build ./workspace/room
```

Verify the build artifacts exist:

```sh
ls workspace/room/target/room-api.yml
ls workspace/room/target/wasm32-unknown-unknown/release/room.wasm
```

The API file (`room-api.yml`) is placed in the top-level target directory.
The compiled binary (`room.wasm`) lives in cargo's standard output path
under `target/wasm32-unknown-unknown/release/`. The controller cell will
import the API file in the next step.

## Step 5 -- Create the controller cell

Scaffold the controller project:

```sh
./workspace/myrmic new ./workspace/controller
```

## Step 6 -- Write the controller cell logic

The controller imports the room cell's API to get a generated `RoomClient`
that can invoke the room's commands. Each controller instance reads its
target room's SRI from its own state (set via instance data).

Open `workspace/controller/src/lib.rs` and replace its contents with:

```rust
#![no_std]

use serde::{Deserialize, Serialize};
use myrmic_sdk::{String, format, Result, macros::{cell, import_cells}};

import_cells!("../room/target/room-api.yml");

#[derive(Default, Serialize, Deserialize)]
struct Controller {
    room_sri: String,
}

#[cell(state_serde = "json")]
impl Controller {
    #[init]
    #[must_use]
    fn init() -> Self {
        Controller {
            room_sri: String::new(),
        }
    }

    #[command]
    fn read_temperature(&self) -> Result<String> {
        let sri: &'static str = self.room_sri.clone().leak();
        let room = RoomClient::new(sri);
        let temp = room.get_temperature()?;
        Ok(format!("Room {}: {}", self.room_sri, temp))
    }
}
```

Key points:

- `import_cells!("../room/target/room-api.yml")` generates `RoomClient`
  from the room cell's API file. The path is relative to the controller
  crate's root directory (where `Cargo.toml` lives). This is why the room
  cell had to be built first -- the API file must exist at compile time.
- `Controller` has a `room_sri` field. Instance data like
  `{"room_sri": "room.001"}` wires this controller to `room.001`.
- `read_temperature` creates a `RoomClient` targeting the room SRI from
  this instance's state, calls `get_temperature`, and formats the result.
  The same binary, given different instance data, talks to different rooms.
- `self.room_sri.clone().leak()` converts the `String` into a
  `&'static str`. The generated `RoomClient::new` requires a `'static`
  reference. When the SRI is a compile-time constant you can pass a string
  literal directly; here it comes from instance data at runtime, so we
  clone and leak.

## Step 7 -- Build the controller cell

```sh
./workspace/myrmic build ./workspace/controller
```

Verify the build:

```sh
ls workspace/controller/target/controller-api.yml
ls workspace/controller/target/wasm32-unknown-unknown/release/controller.wasm
```

As with the room cell, the API file is in the top-level target directory
and the compiled binary is under `target/wasm32-unknown-unknown/release/`.

## Step 8 -- Write the app-spec

The app-spec is a YAML file that describes which cell classes to build and
which instances to deploy, including their instance data.

Create `workspace/app.yml` with the following content:

```yaml
classes:
  - id: room
    build: room
  - id: controller
    build: controller

instances:
  - class: room
    sri: "room.001"
    instance_data:
      degrees_celsius: 20

  - class: room
    sri: "room.002"
    instance_data_file: "room002.json"

  - class: controller
    sri: "controller.001"
    instance_data:
      room_sri: "room.001"
```

Each entry in `instances` references a `classes` entry by id via `class`.
The `sri` field sets the deployed identifier.

Instance data can be provided in two ways:

- **`instance_data`** -- inline JSON, written directly in the YAML. Used
  here for `room.001` and `controller.001`.
- **`instance_data_file`** -- path to a JSON file, relative to the
  app-spec's directory. Used here for `room.002`.

The two fields are mutually exclusive -- specifying both on the same
instance is an error.

## Step 9 -- Write the instance data file

Create `workspace/room002.json` with the initial temperature for `room.002`:

```json
{
  "degrees_celsius": 18
}
```

This file is referenced by the `instance_data_file` field in the app-spec.
The JSON structure must match the cell's state struct -- in this case,
`Room { degrees_celsius: i32 }`.

## Step 10 -- Start the runtime

Start a myrmic runtime in the background:

```sh
./workspace/myrmic runtimes start --detached
```

Verify it is running:

```sh
./workspace/myrmic runtimes list
```

You should see a `default` runtime with status `running`.

## Step 11 -- Deploy via app-spec

Deploy the application using the app-spec:

```sh
./workspace/myrmic deploy ./workspace/app.yml
```

This builds both cell classes (room and controller), creates instances with
the specified instance data, and deploys everything to the running runtime.

Verify all cells are registered:

```sh
./workspace/myrmic status
```

You should see `room.001`, `room.002`, and `controller.001` in the output.

## Step 12 -- Verify the room instances

Each room was initialized with a different temperature via instance data.
Query them to confirm:

```sh
./workspace/myrmic send room.001 get_temperature
```

Output:

```
INFO  20 degrees
```

```sh
./workspace/myrmic send room.002 get_temperature
```

Output:

```
INFO  18 degrees
```

Same cell class, different state -- because each instance received different
instance data at deploy time.

## Step 13 -- Verify the controller

The controller instance `controller.001` was configured (via instance data)
to talk to `room.001`:

```sh
./workspace/myrmic send controller.001 read_temperature
```

Output:

```
INFO  Room room.001: 20 degrees
```

The controller read its `room_sri` from its own state, created a client for
that room, and forwarded the temperature. The same controller binary, given
a different `room_sri`, would talk to a different room -- which is what we
will demonstrate next.

## Step 14 -- Deploy a second controller via CLI

Deploy `controller.002` using the CLI's `--instance-data` flag, binding it
to `room.002`:

```sh
./workspace/myrmic deploy ./workspace/controller/target/wasm32-unknown-unknown/release/controller.wasm \
    --name controller.002 \
    --instance-data '{"room_sri": "room.002"}'
```

This deploys the same controller binary as `controller.001`, but with
different instance data. The `--instance-data` flag accepts inline JSON,
just like the `instance_data` field in the app-spec.

There is also a `--instance-data-file` flag that accepts a path to a JSON
file, mirroring the `instance_data_file` field.

## Step 15 -- Verify the second controller

```sh
./workspace/myrmic send controller.002 read_temperature
```

Output:

```
INFO  Room room.002: 18 degrees
```

Same binary, different behavior: `controller.001` reports `room.001` at
20 degrees, while `controller.002` reports `room.002` at 18 degrees. The
only difference is the instance data each received at deploy time.

## Step 16 -- Cleanup

Stop the runtime and clean up:

```sh
./cleanup.sh
```

This stops the background runtime and removes all generated files from the
workspace.

## Summary

This tutorial demonstrated three ways to provide per-instance data:

| Method                | Where                          | Use case                        |
|-----------------------|--------------------------------|---------------------------------|
| `instance_data`       | inline in app-spec YAML        | small, self-contained configs   |
| `instance_data_file`  | JSON file path in app-spec     | larger configs, version control |
| `--instance-data`     | CLI flag on `myrmic deploy`    | ad-hoc single-cell deploys      |

All three deliver JSON bytes that are deserialized into the cell's state
struct (enabled by `state_serde = "json"` on the `#[cell]` attribute).
Without instance data, the `#[init]` constructor runs as the fallback.
