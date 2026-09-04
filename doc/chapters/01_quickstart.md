# Quickstarts

Myrmic offers a new approach for building distributed edge applications. Instead of building application logic and worrying about connectivity - placement - database - observability - Myrmic handles that. You write application logic as cells: small, self-contained Rust programs that compile to WebAssembly and run on Myrmic runtimes across your device swarm. The focus shifts from infrastructure to logic.

To deliver that, Myrmic provides developers with three things:

- **Myrmic Runtime** - the process you start on a device. It connects to other runtimes in the network peer-to-peer to form a swarm. It runs cells, decides where they are placed in the swarm, routes messages between them, provides a distributed database to store their data, and collects telemetry across the swarm.
- **Myrmic SDK** - a full toolkit to build Myrmic cells and applications from the ground up. It provides macros to define cells and their interfaces, and utilities to communicate between them, interact with the runtime database, schedule tasks, access hardware on embedded targets, and logging.
- **Myrmic CLI** - the command-line interface tool for developers to create, build, and deploy cells, interact with them, manage Myrmic runtimes, and observe the swarm - in development and production.

## Supported Targets

Myrmic Runtime runs on Linux-based systems and a growing set of embedded targets. The following table lists currently supported targets:

| Target | Architecture | Embedded Wasm Engine |
|---|---|---|
| Linux | x86_64 / aarch64 | Wasmtime (JIT) |
| ESP32-C5 | RISC-V (rv32imac) | WAMR (AOT) |
| ESP32-C6 | RISC-V (rv32imac) | WAMR (AOT) |
| ESP32-C61 | RISC-V (rv32imac) | WAMR (AOT) |

## Installation (Linux)

To work with Myrmic, you only need to install the Myrmic CLI. The Myrmic SDK is a Rust dependency you add to your cell code. The Myrmic Runtime is managed by the CLI - no separate installation needed.

For the scope of this page, only **Linux** installation and setup is covered. For embedded targets, see *the Embedded tutorial* (TBD).

### Prerequisites

Cells are written in Rust and the Myrmic CLI relies on Cargo to build them. Building cells requires the Rust nightly toolchain and the WebAssembly target. Make sure the following are installed before proceeding:

- **Rust** - the language cells are written in. Install via [rustup](https://rustup.rs/):
  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

- **Nightly toolchain** - required to compile cells to WebAssembly:
  ```sh
  rustup toolchain install nightly
  ```

- **WebAssembly target** (`wasm32-unknown-unknown`) - the compilation target for cells:
  ```sh
  rustup target add wasm32-unknown-unknown --toolchain nightly
  ```

- **rust-src component** - required by the build process to compile the core library for the WebAssembly target:
  ```sh
  rustup component add rust-src --toolchain nightly
  ```

### Build from Source

Clone the repository and build the CLI:

```bash
git clone https://github.com/peeriot/myrmic.git
cd myrmic
cargo build --release --bin myrmic
```

The binary is at `target/release/myrmic`. Add it to your `PATH` or install it directly to your system:

```bash
cargo install --path swarm/myrmic-cli/
```

To export telemetry - logs, traces, and metrics - to external tools such as Grafana or Jaeger via OTLP, add `--features open-telemetry`:

```sh
cargo build --release --bin myrmic --features open-telemetry
// or
cargo install --path swarm/myrmic-cli/ --features open-telemetry
```

This is covered in the [Observability tutorial](./04_tutorials/06_observability.md).

### Install from a Release Package

`TBD`

### Verify Installation

Confirm the installation succeeded:

```bash
myrmic --version
```

## Your First Cell

In this section you will build your first cell and deploy it to a local runtime, then interact with the cell from the CLI.

### 1. Create a new Cell.

Create a working directory and navigate into it:

```bash
mkdir myrmic-quickstart && cd myrmic-quickstart
```

Then scaffold the cell, run:

```bash
myrmic new counter
```

Expected output:

```text
INFO  Creating 'counter'
```

This command creates a minimal cell as a Rust crate, using a built-in template to quickly get started and remove the complexity of manual setup. The resulting layout looks like this:

```text
counter/
  Cargo.toml   -- crate named "counter"; has the myrmic myrmic-sdk as dependency
  src/
    lib.rs     -- the Cell code lives here
```

### 2. Inspect the generated Cell.

Open `counter/Cargo.toml`. It looks like this:

```toml
[package]
name = "counter"
version = "0.1.0"
edition = "2024"
publish = false

[package.metadata.myrmic]
heap_size = 65_536

[dependencies]
myrmic-sdk = "x.x.x"
```

Most of that is self explaining except:
`heap_size` - which set how much memory this Cell gets: 64 KB, baked into the Wasm binary at build time. See [Cell and application configuration](./10_reference/01_configuration/02_cell-and-application-configuration.md) to understand more.

Now open `counter/src/lib.rs`. This is the starter Cell code that was generated:

```rust
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Callback, JsonValue, Metadata};

const STATE: State<i32> = State::new_const("my-key");

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("starting (id={:?})", md.id).ok();
    Ok(())
}

#[myrmic_sdk::cmd]
fn count(md: Metadata, callback: Callback<JsonValue>) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("returning count to (sender={:?})", md.sender).ok();
    let value = STATE.load()?.unwrap_or_default();
    callback.invoke(md.sender, &JsonValue::from(value))?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn increment(md: Metadata) -> myrmic_sdk::Result {
    let count = STATE.upsert_with(|count| {
        *count = *count + 1;
    })?;
    let _ = myrmic_sdk::info!("Incremented count to {} (sender={:?})", count, md.sender).ok();
    Ok(())
}

#[myrmic_sdk::cmd]
fn decrement(md: Metadata) -> myrmic_sdk::Result {
    let count = STATE.upsert_with(|count| {
        *count = *count - 1;
    })?;
    let _ = myrmic_sdk::info!("Decremented count to {} (sender={:?})", count, md.sender).ok();
    Ok(())
}
```

A few things to note about the code:

- `#![no_std]` - since Myrmic cells are meant to run on any target - including bare-metal devices where the standard library is not available - cells are built without it. The SDK provides what you need in its place.

- `State<i32>` - persistent state stored in the runtime database.

- `#[myrmic_sdk::init]` - marks the init function. Runs once when the Cell is first deployed.

- `#[myrmic_sdk::cmd]` - marks a function as a command handler. Myrmic cells are event-driven: they sit idle until a message arrives. Messages are either commands - a request directed at a cell to perform an action - or events. A function marked with this macro is invoked whenever the Cell receives its matching command. Events are not covered here - see the tutorials or the dedicated guide.

- `Metadata` - holds the invocation context. It is passed to every handler and provides information such as the identity of the current cell and the identity of the sender.

This cell exposes three commands:

- `count` - returns the current count to the caller.
- `increment` - increments the count by 1 and logs the result.
- `decrement` - decrements the count by 1 and logs the result.

### 3. Build the Cell.

Run from the `myrmic-quickstart/` folder:

```bash
myrmic build counter
```

Expected output:

```text
INFO  Attempting to build: .../counter/Cargo.toml
   Compiling counter v0.1.0 (.../counter)
    Finished release [optimized] target(s) in Xs
```

This compiles the Cell to WebAssembly. The binary `counter.wasm` is placed in `counter/target/`.

### 4. Start a local runtime.

The runtime must be running before you can deploy. Start it in a separate terminal and leave it running for the rest of the steps.

```bash
myrmic runtimes start
```

Verify the runtime is running from your original terminal:

```bash
myrmic runtimes list
```

Expected output:

```text
default	running	pid=<pid>
```

This means that we have a local Myrmic runtime running on the machine.

### 5. Deploy to the runtime.

`myrmic deploy` builds and deploys the cell in one step.

```bash
myrmic deploy counter
```

Expected output:

```text
INFO  deploying cell (srn = counter, sri = <uuid>)
INFO  deployed cell (srn = counter, sri = <uuid>)
```

Each cell has two identifiers: an `srn` (Stable Resource Name) - the human-readable name - and an `sri` (Stable Resource Identifier) - a unique identifier used internally by the runtime. The srn is derived from the crate name by default and the sri is derived deterministically from it. The cell is reachable by either.

To use a different SRN, pass `--name`:

```bash
myrmic deploy counter --name other-name
```

### 6. Check what is running.

Lists all Cells deployed to the runtime.

```bash
myrmic cells
```

Expected output:

```text
  cell     sri                                   kind  runtime  class    srn
──────────────────────────────────────────────────────────────────────────────
  counter  xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx  wasm  default  counter  counter
```

`counter` is deployed on the `default` runtime and waiting for commands.

### 7. Call `increment`.

`myrmic send` sends a command to a cell - it takes the cell identifier and the command name. Call `increment` three times:

```bash
myrmic send counter increment
myrmic send counter increment
myrmic send counter increment
```

### 8. Check the logs.

The cell logs its state after every increment. Check what happened:

```bash
myrmic telemetry logs
```

Look for these lines in the output:

```text
Incremented count to 1 (sender=...)
Incremented count to 2 (sender=...)
Incremented count to 3 (sender=...)
```

### 9. Remove the Cell.

```bash
myrmic delete counter
```

Removes the Cell from the runtime.

Expected output:

```text
INFO  undeployed cell 'counter' (sri xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)
```

Verify it is gone:

```bash
myrmic cells
```

Expected output:

```text
No cells registered
```

### 10. Stop the runtime when you are done.

```bash
myrmic runtimes stop
```

Expected output:

```text
INFO  sent SIGTERM to runtime "default" (pid <pid>)
```

## Getting Help

At any point, if you get stuck, you can always get help on
[Discord](https://discord.gg/zExh79pWgj) or
[GitHub Discussions](https://github.com/peeriot/myrmic/discussions).
No question is too small - the community is here for all of them.

## Next Steps

- [Tutorials](./04_tutorials.md) - hands-on walkthroughs to go further.
- [Guides](./05_guides.md) - learn individual Myrmic features and concepts.
