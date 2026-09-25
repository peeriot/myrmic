<p align="center">
  <a href="https://hackthething.io">
    <img src="https://img.shields.io/badge/%F0%9F%8F%86%20Build%20%2F%20Break%20Challenge-%E2%82%AC3%2C000%20prize%20pool-8A2BE2?style=for-the-badge" alt="Build / Break Challenge - &euro;3,000 prize pool">
  </a><br>
  Help us make Myrmic better: build something with it, break it, or extend it - and share what you find.<br>
  &euro;3,000 prize pool as a thank you. Open until <b>11 October 2026</b>.<br>
  <b>&rarr; <a href="https://hackthething.io">Join the challenge</a> &larr;</b>
</p>

<p align="center">
  <a href="https://book.myrmic.dev">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="doc/images/myrmic-logo-dark.svg">
      <source media="(prefers-color-scheme: light)" srcset="doc/images/myrmic-logo.svg">
      <img alt="Myrmic" src="doc/images/myrmic-logo.svg" width="320">
    </picture>
  </a>
</p>

<h2 align="center">write cells, not infrastructure</h2>

<p align="center">
  <a href="https://book.myrmic.dev"><img src="https://img.shields.io/badge/docs-book-blue.svg?style=flat-square" alt="Book"></a>
  <a href="https://docs.rs/myrmic-sdk"><img src="https://img.shields.io/badge/docs-docs.rs-blue.svg?style=flat-square" alt="docs.rs"></a>
  <a href="https://github.com/peeriot/myrmic/releases"><img src="https://img.shields.io/github/v/release/peeriot/myrmic?filter=myrmic*&label=release&style=flat-square" alt="Latest release"></a>
  <a href="https://crates.io/crates/myrmic-sdk"><img src="https://img.shields.io/crates/d/myrmic-sdk?style=flat-square" alt="Downloads"></a>
  <a href="#status"><img src="https://img.shields.io/badge/status-experimental-orange.svg?style=flat-square" alt="Status: experimental"></a>
  <a href="rust-toolchain.toml"><img src="https://img.shields.io/badge/rust-1.97-orange.svg?style=flat-square" alt="Rust 1.97"></a>
  <a href="https://discord.gg/zExh79pWgj"><img src="https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white&style=flat-square" alt="Discord"></a>
  <a href="https://www.youtube.com/@MyrmicOfficial"><img src="https://img.shields.io/badge/YouTube-red?logo=youtube&logoColor=white&style=flat-square" alt="YouTube"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-GPL--2.0%20%2B%20Myrmic%20Exception-blue.svg?style=flat-square" alt="License: GPL-2.0 with Myrmic Exception"></a>
</p>

<div align="center">
  <h3>
    <a href="https://myrmic.dev">Myrmic Site</a>
    <span> | </span>
    <a href="https://book.myrmic.dev">Docs Site</a>
    <span> | </span>
    <a href="https://docs.rs/myrmic-sdk">Rust Docs</a>
    <span> | </span>
    <a href="https://hackthething.io">Build / Break Challenge</a>
  </h3>
</div>
<br/>

## What is Myrmic?

**Myrmic** is an open-source runtime written in Rust for distributed edge applications, built for heterogeneous environments with limited resources and connectivity. Myrmic manages messaging, state, placement and recovery locally, shifting your focus from infrastructure to application logic, under one programming model across different targets.

**You write cells.** A cell is a small, stateful Rust module compiled to WebAssembly, so it runs in an isolated sandbox. It has its own identity, a mailbox, and handlers for the commands and events it cares about.

**You start a runtime on each machine.** A machine running the Myrmic runtime is a node - a server, a Raspberry Pi, an ESP32. Nodes on the same network discover each other and form a swarm, each advertising what it can offer.

**You deploy your cells.** You say what each one requires, and the node swarm decides where each cell runs. The orchestration happens locally, without a central cloud controller making decisions.

**Once your cells are deployed,** the runtime routes messages between them and calls the right cell handler when a matching message arrives. State and data the cells write are persisted in a database distributed across the swarm's nodes, under scopes you choose: private to one cell, or shared between them.

**Resilience.** When a cell or a node goes away, the swarm reports it and can bring the cell back on another node, with its state if it was replicated.

**When your cells interact with hardware,** Myrmic adds the **Signal Layer**: native Rust that runs besides the runtime on the node at full speed, outside the sandbox. Cells talk to it to read sensors and drive actuators.

Myrmic is in **Developer Preview**: it works, people use it, and its APIs will still change.

Help us make Myrmic better: join the [Build / Break Challenge](https://hackthething.io). Build something with Myrmic, break it, or extend it. Open until **11 October**, with a 🏆 **&euro;3,000 prize pool** as a thank you.

## Getting started

Myrmic takes on the infrastructure work - connectivity, placement, storage, observability - so that what you write is application logic. To do that, it offers three things.

**Myrmic Runtime** - what you run. The process you start on a device. It joins the other runtimes peer-to-peer to form a swarm, runs your cells, decides where they are placed, routes the messages between them, stores their data, and collects telemetry.

**Myrmic SDK** - dependency on your code, what you build with. Macros to define cells and their interfaces, and utilities to send messages, work with the runtime database, schedule tasks, reach hardware on embedded targets, and log.

**Myrmic CLI** - what you install. One tool to create, build and deploy cells, talk to them, start and manage runtimes, and observe the swarm, in development and in production. It includes the runtime, so you do not install that separately.

### Supported targets

The Myrmic Runtime runs on Linux and a growing set of embedded targets.

| Target    | Architecture         | Wasm engine    | Cells per node | Heap left for the cells         |
|-----------|----------------------|----------------|----------------|--------------------------------|
| Linux     | x86_64 / aarch64     | Wasmtime (JIT) | many           | no fixed limit                 |
| ESP32-C5  | RISC-V (riscv32imac) | WAMR (AOT)     | one            | 184 KB, plus up to 8 MB PSRAM  |
| ESP32-C6  | RISC-V (riscv32imac) | WAMR (AOT)     | one            | 336 KB                         |
| ESP32-C61 | RISC-V (riscv32imac) | WAMR (AOT)     | one            | 160 KB, plus up to 2 MB PSRAM  |

### Installation

- [Installing on Linux](https://book.myrmic.dev/docs/quickstart/installation) - the supported ways to install the Myrmic CLI: from a release package (`.deb` or `.rpm`) on x86_64, or from source on arm64. Covers the prerequisites for each path, and the prerequisites for building cells.
- [Installing on Embedded (ESP32)](https://book.myrmic.dev/docs/quickstart/installation-embedded) - how to set up the Myrmic Runtime on an ESP32 device: build it as firmware on your Linux machine, then flash it to the device. Also covers the prerequisites.

### Your first cell (Linux)

This assumes that the Myrmic CLI is installed on your Linux machine, with all the prerequisites needed to work with Myrmic in place.

Start by scaffolding a cell:

```bash
mkdir myrmic-quickstart && cd myrmic-quickstart
myrmic new counter # scaffolds a Rust crate with myrmic-sdk as a dependency
```

The generated cell:

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
fn count(md: Metadata, callback: Option<Callback<JsonValue>>) -> myrmic_sdk::Result {
    let value = STATE.load()?.unwrap_or_default();

    // `myrmic send` carries no callback, and a nil sender that could not be
    // answered even if it did.
    if let Some(callback) = callback
        && !md.sender.is_nil()
    {
        let _ = myrmic_sdk::info!("Returning count {} to (sender={:?})", value, md.sender).ok();
        callback.invoke(md.sender, &JsonValue::from(value))?;
    } else {
        let _ = myrmic_sdk::info!("Count is {} (no caller to answer)", value).ok();
    }

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

In a second terminal, start a Myrmic runtime and look at your one-node swarm:

```bash
myrmic runtimes start
myrmic network status
```

Back in the first terminal, build the cell, deploy it, and talk to it:

```bash
myrmic build counter
myrmic deploy counter
myrmic send counter increment
myrmic telemetry logs         # look for "Incremented count to 1"
```

`myrmic delete counter` and `myrmic runtimes stop` clean up.

### Next steps

- [Quickstart](https://book.myrmic.dev/docs/quickstart) - the counter cell example you just ran, explained in depth.
- [Concepts](https://book.myrmic.dev/docs/concepts) - introduce the Myrmic model and the concepts it is made of.
- [Tutorials](https://book.myrmic.dev/docs/tutorials) - show you how to use Myrmic hands-on, including working with embedded devices and the Signal Layer.
- [Guides](https://book.myrmic.dev/docs/guides) - walk you through Myrmic's features, explaining what each one does and showing how to use it with code snippets.
- [Cell and application examples](examples/) - complete cell and application examples you can read, run and adapt.
- [ESP32 firmware examples](embedded/esp-hal/firmware-examples/) - for customising the runtime firmware you flash to an ESP32, when the default one does not do what you need.

## Status

What holds today, and what the roadmap adds next.

| Area | Today | Next |
|---|---|---|
| Targets | Linux (x86_64/aarch64); ESP32-C5, C6, C61 | macOS, Windows, nRF5340 |
| Placement | Capability tags; all-or-nothing deployment with rollback | Quorum-based leadership with fencing (Partition Tolerance) |
| Node loss | Lost nodes are detected and reported after about a minute; cells with a restart policy can be brought back on another qualifying node | The responsibility to run that cell moves to another node in seconds |
| Cell state | One copy, survives a runtime restart; replication across nodes is explicit admin configuration, and unreplicated data is lost with its node | Quorum-held state and mailboxes: kill a node, keep the cell (Durability) |
| Messaging | Tracked delivery to the node where the cell runs, plus best-effort delivery. Strict ordering per cell, none across cells. One transaction per command handler covering mailbox, state and outbound messages | Durable mailboxes |
| Isolation | WebAssembly sandbox, cells only reach host functions they were granted | Time and throughput isolation |
| Security | Assumes a trusted network; no cryptographic sender proof between nodes yet | Authenticated fabric, membership reconfiguration, upgrade semantics (Secure Swarm) |
| APIs | Change without notice until the API freeze | Stable SDK and CLI |

- [Guarantees](https://book.myrmic.dev/docs/guarantees) - more details on what holds today and what is coming.
- [Roadmap](https://book.myrmic.dev/docs/roadmap) - more details on the stages.

Four rules for writing cells with API we have today:

- Make every cell handler idempotent, so it is safe to run twice.
- Treat a missing callback as unknown rather than failed.
- Assume a single copy of state, unless you configured otherwise.
- Messages between cells can arrive in any order - add your own ordering logic if you need it.

## Where Myrmic fits

**Not a message broker.** Cells have identity, state and placement; messaging is one part of the runtime, not the product. Myrmic can talk to MQTT and HTTP from inside a cell.

**Not a Kubernetes distribution.** No control node, no container images, no Linux requirement. A node can be a microcontroller with 160 KB of heap.

**Not only a WebAssembly runtime.** Wasmtime on Linux and WAMR on microcontrollers run underneath. Myrmic adds identity, mailboxes, state, placement and restart on top.

**Not only an MCU framework.** Embassy powers the microcontroller targets. Myrmic makes those devices full members of the same swarm as your servers.

If you know Erlang/OTP: a cell is close to a supervised actor with a mailbox. The difference is that the supervisor is the swarm, and a node can be an ESP32.

## Scope and limits

Myrmic is designed for soft real-time coordination where 10-500 ms of latency jitter is acceptable, and for non-critical processes only.

- **Soft real-time only.** No deterministic deadlines under 1 ms, no TSN or isochronous synchronization.
- **Non-critical processes only.** Safety-critical control is expressly excluded. There is no certified safe-state mechanism, and GPIO state on crash is undefined.
- **Availability over consistency.** AP under CAP. Transactions needing atomic real-time consistency are not supported.
- **No functional-safety guarantees.** High-availability patterns such as 1oo2 and 2oo3, not fault tolerance beyond that.

Use outside these limits is at your own risk.

## Repository layout

| Directory | Contents |
|---|---|
| [`swarm/`](swarm/) | OS-targeted runtime and platform crates, including the `myrmic` CLI |
| [`embedded/`](embedded/) | SoC-specific implementations, Espressif first |
| [`sdk/`](sdk/) | WebAssembly SDK and example cells |
| [`doc/`](doc/) | Content for the [documentation site](https://book.myrmic.dev), in markdown files |

## Contribute to Myrmic

Until the API freeze, the most valuable contribution is helping us build, test and improve Myrmic.

- **Build something:** Create an app, hardware integration or experiment and share it with the community.
- **Find something broken:** [Open an issue](https://github.com/peeriot/myrmic/issues) with a minimal repro and logs.
- **Something was confusing:** Found something unclear in the docs, CLI, SDK or error messages? [Start a discussion](https://github.com/peeriot/myrmic/discussions).
- **Questions and show-and-tell:** [Discord](https://discord.gg/zExh79pWgj) or [Discussions](https://github.com/peeriot/myrmic/discussions).
- **Security issues:** Please report them privately as described in [SECURITY.md](SECURITY.md).
- **Code contributions:** Welcome. Please open a discussion first so we can point you toward areas that are actively being developed. See [CONTRIBUTING.md](CONTRIBUTING.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).


## License

Three rules cover most cases:

- **Your code stays your code** as long as it talks to Myrmic through the official interfaces. Closed-source and commercial cells are fine.
- **If you change the Myrmic platform itself, share the change** under the same license, or talk to us about a commercial license.
- **Obligations arise only when you distribute.** Internal use, testing and evaluation create none.

| Component | License |
|---|---|
| Platform: runtime, data layer, firmware, interfaces | GPL-2.0-only with the Myrmic Exception |
| SDK, interface crates, code generators, drivers | MIT OR Apache-2.0 |
| Examples, tutorials, templates | MIT-0 |
| Documentation | CC-BY-4.0 |

- [LICENSING.md](LICENSING.md) - a plain-language overview.
- [`LICENSES/`](LICENSES/) - the full license texts.
- [`EXCEPTION-SCOPE.md`](EXCEPTION-SCOPE.md) - the interfaces covered by the Exception.
- [Licensing](https://book.myrmic.dev/docs/licensing) - more detail: the reasoning behind the rules, a decision guide for your own case, and the CLA.

## About Peeriot

Myrmic is built and maintained by [Peeriot](https://peeriot.io). A commercial enterprise edition with fleet operations and support is planned; the runtime stays open source.

"Myrmic", "EdgeVance", and "Peeriot" are trademarks of Peeriot GmbH. The Myrmic logo and other brand assets are not covered by the licenses applicable to the software or documentation.

---

<p align="center">
  Copyright © Peeriot GmbH · <a href="mailto:contact@myrmic.dev">contact@myrmic.dev</a><br>
  Contains third-party code under separate licenses. See <a href="NOTICE">NOTICE</a>.
</p>
