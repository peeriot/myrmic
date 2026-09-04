# myrmic Tutorial

This tutorial walks through the full cell lifecycle -- scaffolding, building,
deploying, inspecting, invoking, and deleting -- using `myrmic`, the
user-facing CLI for the myrmic swarm.

`myrmic` combines the workflows shown in the `cell-tools` and `cell-ctl`
tutorials into a single tool, and adds a `new` command for bootstrapping a
cell project from a template. The binary is called `myrmic`; the crate is
`myrmic-cli`.

You will:

- Scaffold a new cell project with `myrmic new`
- Build the cell with `myrmic build` (produces `<name>.wasm` and `<name>-api.yml`)
- Start a swarm and deploy the cell with `myrmic deploy`
- List registered cells with `myrmic status`
- Invoke commands on the cell with `myrmic send`, passing JSON payloads
- Remove the cell with `myrmic delete`

All commands assume you are running from the
`swarm/tutorials/myrmic-cli/` directory.

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

Due to current company policy, you will need SSH access to `github.com`.
You can find out how to do this [here](https://docs.github.com/en/authentication/connecting-to-github-with-ssh).

## Step 1 -- Setup

Run the setup script to build all required binaries into the workspace:

```sh
./setup.sh
```

This produces the following in `workspace/`:

```
workspace/swarm             -- the swarm runtime
workspace/myrmic            -- the myrmic CLI
```

## Step 2 -- Scaffold a cell

Bootstrap a new cell project called `counter` inside the workspace:

```sh
./workspace/myrmic new ./workspace/counter
```

`myrmic new` uses a built-in template to produce a minimal cell crate.

The resulting layout looks like this:

```
workspace/counter/
  Cargo.toml   -- crate named "counter"; depends on myrmic-sdk + serde
  src/
    lib.rs     -- a default #[cell] impl with get_count / summary / increment commands
```

## Step 3 -- Inspect the scaffolded cell

Open `workspace/counter/src/lib.rs` — the file `myrmic new` generated for
you. It looks like this:

```rust
#![no_std]

use serde::{Deserialize, Serialize};
use myrmic_sdk::{String, format, macros::cell};

#[derive(Default, Serialize, Deserialize)]
struct Counter {
    count: i32,
}

#[cell]
impl Counter {
    #[init]
    #[must_use]
    fn init() -> Self {
        Counter { count: 0 }
    }

    /// Returns the current count.
    #[command]
    fn get_count(&self) -> i32 {
        self.count
    }

    /// Returns a human-readable summary of the current count.
    #[command]
    fn summary(&self) -> String {
        format!("Count({})", self.count)
    }

    #[command]
    fn increment(&mut self, value: i32) {
        self.count += value;
    }
}
```

Key points:

- The `#[cell]` macro generates the init, command dispatch, and state
  (de)serialization for you. `#[init]` marks the constructor; `#[command]`
  marks each RPC-style entry point.
- The cell exposes three commands: `get_count` returns the raw `i32`,
  `summary` wraps it in a human-readable `String`, and `increment` takes
  an `i32` and updates the count.
- `myrmic send` encodes payloads and invokes commands on cells from the CLI.

## Step 4 -- Build the cell

Build the cell via `myrmic`:

```sh
./workspace/myrmic build ./workspace/counter
```

`myrmic build` packages your cell up as a `.wasm` binary, as well as a `-api.yml`
file that fully describes your cell's commands, events, and domain types.

You can find these in the `target/` directory.
Verify the artifacts exist:

```sh
ls workspace/counter/target/
```

You should see `counter.wasm` and `counter-api.yml`.

> **Tip:** Use `--persist-build-dir` to keep the temporary wrapper
> directory around for inspection, or `--tmp-dir <path>` to place it in a
> specific location. Use `--wasm` or `--api` on their own to build only
> one of the two artifacts.

## Step 5 -- Start the swarm

Start the swarm runtime in the background (or in a separate terminal).
The tutorial includes a `swarm-config.jsonnet` that configures a
single-node swarm.

```sh
RUST_LOG=sorg_execution=info ./workspace/swarm ./swarm-config.jsonnet &
SWARM_PID=$!
sleep 3
```

If you prefer an interactive session, run the same command in a separate
terminal without the trailing `&` and `SWARM_PID=$!` line, then continue
in your original terminal.

Verify the swarm is running:

```sh
./workspace/myrmic status
```

You should see no entries in the output at this point.

## Step 6 -- Deploy the cell

Deploy the counter cell:

```sh
./workspace/myrmic deploy ./workspace/counter
```

`myrmic deploy` submits the cell to the runtime for execution.
This cell becomes reachable via the SRI.

The SRI is derived automatically from the name, which can be overridden with `--name <name>` if you want something different:

```sh
# optional alternative
# ./workspace/myrmic deploy ./workspace/counter --name my_counter
```

Verify the cell is registered:

```sh
./workspace/myrmic status
```

You should now see two entries: your `counter` (type `Cell`) and the exec
module (type `Exec`).

## Step 7 -- Send commands

`myrmic send` takes three positional arguments and one optional flag:

```
myrmic send <sri> <command> [payload]
            [--raw]        # decode the payload as hex and send the raw bytes
```

The payload is encoded as JSON by default: it is parsed as JSON, and anything
that isn't valid JSON is sent as a JSON string. `--raw` instead decodes the
payload as a hex string and sends those bytes verbatim.

### Query the current count

Sending the command `summary` will return a readable representation:

```sh
./workspace/myrmic send counter summary
```

Output:

```
INFO  Count(0)
```

### Increment

Send the `increment` command with the payload of `5`:

```sh
./workspace/myrmic send counter increment 5
```

Output:

```
INFO  (no response)
```

The `(no response)` line is the CLI reporting that `increment` returned
nothing. Repeat with a different value:

```sh
./workspace/myrmic send counter increment 3
```

### Confirm the state persisted

```sh
./workspace/myrmic send counter summary
```

Output:

```
INFO  Count(8)
```

The cell state advanced from `0` to `5` to `8` across the two increments,
and the swarm persisted the change between command invocations.

## Step 8 -- Delete the cell

Remove the cell:

```sh
./workspace/myrmic delete counter
```

Verify it is gone:

```sh
./workspace/myrmic status
```

You should see an empty output.

## Step 9 -- Cleanup

When you are done, run the cleanup script:

```sh
./cleanup.sh
```

This stops the background swarm process (if any) and removes all generated
files from `workspace/`. If you started the swarm in a separate terminal,
stop it with Ctrl-C before running cleanup.

## What else `myrmic` can do

This tutorial covers the common single-cell workflow. `myrmic` also
supports:

- **App suites.** `myrmic` supports application bundles which package
  multiple cells together into a single "nest", which can be
  handed off as a single deployment artifact. Delete a whole bundle
  with `myrmic delete <name> --app`; use `--cell` to remove a single
  cell. Without a flag, `myrmic delete` refuses a target that is part
  of a multi-cell app so you say which you mean.
- **HTTP gateway.** `myrmic gateway --port 8080` deploys a gateway
  that exposes your deployed cells over HTTP.

Run `./workspace/myrmic <subcommand> --help` to see the flags for each
command.
