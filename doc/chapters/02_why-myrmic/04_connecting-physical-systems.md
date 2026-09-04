# Connecting Physical Systems

Portable application logic and hardware-near execution have different needs.

Myrmic keeps product and application behaviour in WebAssembly Cells while drivers, signal processing, and physical I/O run as native Rust.

> **WebAssembly is the application boundary, not the hardware driver model.**

## The Signal Layer

The Signal Layer is Myrmic's native subsystem for:

- sensor acquisition,
- actuator control,
- driver lifecycle and recovery,
- local filtering and transformation,
- named taps and typed outlets.

It runs on both Node classes:

- as a native process connected through IPC on OS-based Nodes,
- as native tasks using shared memory on MCU-based Nodes.

## Flow-based authoring, native execution

A pipeline describes sources, processing steps, taps, and outlets in YAML.

```text
source → processing steps → tap → Cell
Cell → outlet → native driver → actuator
```

A generator combines the pipeline with the board or Node description, emits Rust, and compiles it into the target.

The graph is not interpreted at runtime. Processing steps execute inside native source tasks without a separate task or message hop for every step.

> **Describe the flow declaratively. Run it as native code.**

Myrmic does not currently include a graphical pipeline editor.

## Taps and outlets

A **tap** is a named, typed value or event exposed to Cells.

An **outlet** is a named, typed command endpoint consumed by a native output driver.

Cells can depend on semantic contracts such as:

```text
room.temperature
motor.status
fan.speed
```

The Cell does not need to know which sensor, bus, board, or driver implements the contract.

## Signal Layer modules become Node Capabilities

A compiled pipeline is installed with a specific Node. Its taps, outlets, protocols, and native functions become Capabilities that Node advertises.

Cells declare capability tags. Placement brings the Cell to a compatible Node.

This makes physical location explicit without leaking concrete hardware into application logic.

## Bare-metal Nodes are first-class participants

A supported MCU can:

- execute WebAssembly Cells compiled ahead of time,
- run the Signal Layer directly on bare metal,
- expose local sensor, actuator, radio, and protocol Capabilities,
- communicate with Cells across the swarm.

The MCU does not need to run the complete platform. Data, orchestration, and gateway roles can live on other Nodes.

## From local signals to application state

A useful separation is:

```text
sensor or actuator
        ↓
native Signal Layer
        ↓
Adapter Cell placed by capability tags
        ↓ Commands and Events
Asset or Agent Cells
```

The local Adapter stays close to the physical Capability. Canonical state, policy, and coordination can remain independent of that hardware location.

## Preferred integration path

New GPIO- and I²C-based integrations should use Signal Layer drivers, taps, and outlets. Direct low-level runtime interfaces remain compatibility or escape-hatch mechanisms rather than the default application architecture.

BLE is broader than a single signal pipeline and may remain a connectivity Capability.

## Scope

Myrmic is not a PLC, hard-real-time controller, or certified functional-safety system. Keep safety-critical control and guaranteed safe-state behaviour in systems designed and certified for them.

## See also

- [Cell Patterns](../06_concepts/07_cell-patterns.md) - recurring designs for combining Cells, Adapters, Agents, and Bridges
- [The Node View](../07_architecture/02_the-node-view.md) - how a Node runs native Rust and WebAssembly side by side
