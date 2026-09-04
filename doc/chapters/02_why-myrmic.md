---
sidebar_label: Why Myrmic
---

**A NEW CATEGORY OF APPLICATION RUNTIME**

# Build software where the world actually happens.

Software is moving closer to where people live and work, into products, machines, buildings, and local operations. But shipping reliable applications for this world still means combining runtimes, messaging, state, placement, hardware integration, and device-specific stacks.

We believe this needs a new category of software, built on a different set of assumptions:

- Physical entities should be active participants in software systems, not merely passive data sources for remote applications.
- Bare-metal microcontrollers (MCUs) should be first-class participants in distributed applications.
- Resilience should be an out-of-the-box capability, not a complex infrastructure task.
- Portable application logic should coexist with native, hardware-optimized execution.
- Software should organize, coordinate, and recover itself, without requiring a third-party operator to keep it running.
- Infrastructure should remain open, inspectable, and under the control of the people who run it.

Myrmic turns these principles into an **open, edge-first application runtime**.

Edge-first means starting with where data, users, devices, and physical processes actually are. It does not mean edge-only. Local, central, and cloud infrastructure all remain valid wherever they fit the application.

Myrmic applications are built from **Cells**: small, stateful units of computation with stable identities. A **Node** is a device or operating-system process running Myrmic. Myrmic addresses each Cell by a **Stable Resource Identifier (SRI)**, not by the Node currently running it. A **Command** asks one Cell to act. An **Event** announces something that happened. Both reach Cells through **Mailboxes**, which hold Messages until a Cell handles them. A Cell also owns **State** and declares the **Capabilities** it needs. A Capability is a function or resource provided by a Node.

> **Build around the Cell, not the Node that happens to run it.**

> **Developer Preview**
> Myrmic is ready to explore, test, and challenge. Stable Cell identity, mailbox-based messaging, placement by capability tags, mixed Linux and MCU deployments, WebAssembly isolation, and native signal processing are available today. Current failure behaviour is deliberately conservative: when a Node is lost, affected deployments are removed and reported. Cell state and mailboxes are held as one copy by default.
> [Read what Myrmic guarantees today](./08_guarantees.md)

## One swarm. Different devices. One application environment.

A Myrmic **swarm** is a network of Nodes that behaves as one application environment. The **Signal Layer** is Myrmic's native layer for acquisition, processing, and control.

![Cells distributed across OS-based and MCU-based Nodes in one Myrmic swarm](../images/swarm-view.svg)

- OS-based Nodes can execute Cells and provide orchestration, data services, gateways, and native services.
- MCU-based Nodes can execute Cells compiled ahead of time. They can also expose specialized signal processing through the Signal Layer.
- Cells are addressed by a Stable Resource Identifier, or **SRI**, without knowing where they run.
- A **Capability** is a function or resource a Node provides. Capability tags connect Cell requirements to eligible Nodes.

## What Myrmic provides today

### A stateful Cell model

Cells combine identity, logic, state, Commands, Events, timers, and lifecycle. Each Cell handles one ordered stream of work, one handler at a time.

### Mailbox-based messaging

Commands and Events pass through named mailboxes. Tracked delivery keeps pursuing a message until it reaches the destination Cell's current mailbox. Best-effort delivery is tried once. The current durability boundary is stated on the [Guarantees](./08_guarantees.md) page.

### Placement by capability tags

Nodes advertise what they provide. Cells declare capability tags. Myrmic places Cells only on Nodes that satisfy those requirements.

### Linux and bare-metal participation

OS-based Nodes use Wasmtime. Supported MCU-based Nodes use WAMR with modules compiled ahead of time. Both participate in the same peer-to-peer fabric and use the same Cell model.

### Native physical integration

The Signal Layer runs natively on both Node classes. It connects drivers, signal processing, taps, and outlets while application logic remains isolated in WebAssembly.

### Predictable failure behaviour

The preview does not claim that a Cell automatically continues after Node loss. Myrmic removes and reports the affected deployment. Cell state and mailboxes use one copy by default, so losing the Node that holds them can also lose that data.

## What the roadmap adds

Myrmic is building toward **Sovereign Cells**: Cells that outlive the Node they run on, because their authority and committed data no longer depend on one Node.

Two properties deliver this capability:

- **Partition Tolerance** keeps the Swarm working correctly when a Node disappears or the network splits. For every Cell there is always exactly one place that may decide and serve — never none, and never two.
- **Durability** keeps everything a Cell has acknowledged, so the Cell can resume on another eligible Node with exactly the State it had.

> **Basic Cell + Partition Tolerance + Durability = Sovereign Cells.** Partition Tolerance alone preserves the decision about who may serve; only with Durability does the Cell's State itself survive.

A last stage, **Secure Swarm**, adds what a production Swarm needs beyond surviving failures: authenticated Nodes and Cells, Nodes that join and leave while Cells keep running, and defined upgrade semantics.

The roadmap is organized around guarantees and reproducible demonstrations, not dates.

[Explore the roadmap](./09_roadmap.md)

## Follow the system from purpose to operation

### [Rethinking the Stack](./02_why-myrmic/01_rethinking-the-stack.md)

**Why does this need to exist?**
Understand the shift toward software that runs closer to its users, data, and physical context, and why the current stack makes that unnecessarily difficult.

### [Understanding Myrmic](./02_why-myrmic/02_understanding-myrmic.md)

**What is the product?**
See how Cells, Nodes, the swarm, Capabilities, native execution, and the six architecture layers form one coherent system.

### [Modelling Applications](./02_why-myrmic/03_modelling-applications.md)

**How do I model applications?**
Learn how Applications, Cells, stable identity, Commands, Events, callbacks, state, Capabilities, and Cell patterns fit together.

### [Connecting Physical Systems](./02_why-myrmic/04_connecting-physical-systems.md)

**How does it reach the physical world?**
Connect portable application logic to sensors, actuators, drivers, and local processing through the native Signal Layer.

### [Operating Myrmic](./02_why-myrmic/05_operating-myrmic.md)

**How does it run in practice?**
Understand Node roles, placement, data locations, gateways, what happens after Node loss, observability, and the path to Sovereign Cells.

## Start with your problem

Different teams enter Myrmic through different needs: modelling physical assets, building connected products, running real-time rooms, coordinating workflows and agents, or providing a stateful platform primitive.

[Explore the target groups](./03_target-groups.md)

## Put the preview to work

Start with one Cell. Run a mixed swarm. Connect a real signal pipeline. Stop a Node and compare what happens with the published guarantee contract.

The preview is the right time to expose unclear APIs, missing Capabilities, weak operational behaviour, and unsupported topologies.

> **Run it. Challenge it. Reproduce the claims that matter to you.**

## See also

- [Core Concepts](./06_concepts.md) - the vocabulary every other page assumes
- [Architecture](./07_architecture.md) - how Myrmic actually works, view by view
- [Guarantees](./08_guarantees.md) - what the current release promises, and what it doesn't yet
- [Roadmap](./09_roadmap.md) - what future stages are intended to add
