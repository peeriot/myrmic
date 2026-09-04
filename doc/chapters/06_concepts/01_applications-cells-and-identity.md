# Applications, Cells, and Identity

## Application

An **Application** is the boundary a developer declares and reasons about. It includes Cell classes, instances, bridges, assets, and deployment configuration.

The boundary carries no runtime guarantee by itself. Cross-Application dependency management is not currently defined.

## Cell

A **Cell** is a self-contained unit of logic, state, and identity.

It is the smallest thing Myrmic deploys, addresses, and remembers.

### Stable logical identity

An Application can give a Cell a human-readable **Stable Resource Name (SRN)**. Myrmic derives a fixed-size **Stable Resource Identifier (SRI)** from that name.

Callers use the SRI, not a network address, port, or Node name. Identity and location are separate concepts.

### WebAssembly module

Cell logic is written in Rust and compiled to WebAssembly.

OS-based Nodes execute it with Wasmtime. MCU-based Nodes execute modules compiled ahead of time with WAMR. The Node exposes only the functions allowed by the Cell's Capabilities.

### Named handlers

A Cell exposes Command and Event handlers discovered from the module. Timers can wake named handlers later or periodically.

### Owned state

A Cell owns state under its SRI. State is what the Cell remembers between Messages. The current durability contract is defined on the [Guarantees](../08_guarantees.md) page.

## Class and instance

A **Cell Class** is the deployable definition: the WebAssembly binary, build targets, capability requirements, and initialization contract.

A **Cell instance** is one running identity created from that class, with its own SRI and state.

## Names and identifiers

The mapping from SRN to SRI is deterministic. This means:

- the Application chooses the name,
- the derivation needs no central naming registry,
- any participant that knows the SRN can compute the same SRI,
- a Cell can address another Cell it has never previously discovered.

## Creation and lifetime

Any Cell may create another Cell.

An optional **lease** can make the created Cell's lifetime depend on its creator. This is a strict lifetime relationship, not a supervision or recovery guarantee.

## See also

- [Nodes, Capabilities, and the Swarm](./02_nodes-capabilities-and-the-swarm.md) - where Cells run and how placement is decided
- [Core Concepts](../06_concepts.md) - the full list of concepts this page belongs to
