# Understanding Myrmic

Myrmic is an open runtime for stateful edge applications that span servers, local systems, and physical devices.

A **Cell** is a small, stateful program with a stable identity. Myrmic combines this Cell model with Mailbox messaging and placement based on capability tags. WebAssembly provides isolated execution, native Rust handles signal processing, and peer-to-peer sessions connect the runtime.

> **One application model across a mixed swarm of OS-based systems and bare-metal MCUs.**

## The swarm is the product

![The Myrmic swarm view](../../images/swarm-view.svg)

A **Node** is a device or operating-system process running Myrmic. A **swarm** is a network of Nodes that behaves as one application environment.

Cells address each other by a **Stable Resource Identifier (SRI)** rather than a network location. A **Capability** is a function or resource a Node provides. Cells declare capability tags, and placement selects an eligible Node.

## Three concerns stay separate

Most stateful systems fuse authority, data, and execution. Myrmic treats them as separate concerns:

| Concern | Question |
| --- | --- |
| **Authority** | Who may decide placement, membership, and the serving role? |
| **Data** | Where do state, mailboxes, and records live, and what survives? |
| **Execution** | Which Node is currently running the Cell? |

The roadmap introduces the concept of a **Sovereign Cell**: a Cell that survives its Node. This is achieved by replicating **Authority** and **Data**, never by replicating **Execution** — the system runs one active Cell instance, not several copies executing in lockstep.

## The six layers

Myrmic uses one canonical architecture model:

1. **Execution Layer:** WebAssembly Cell execution
2. **Self-Organization Layer:** membership, authority, and placement
3. **Data Layer:** state, mailboxes, and records
4. **Peer-to-Peer (P2P) Layer:** the shared session and protocol fabric
5. **Transport Layer:** physical network links
6. **Signal Layer:** native acquisition, processing, and control

[See the six-layer view](../07_architecture/05_the-six-layers.md)

## The application surface

Developers interact with Myrmic through:

- the Rust Cell SDK,
- Application manifests,
- the `myrmic` CLI,
- Commands, Events, and callbacks,
- the WebSocket gateway and packaged web assets,
- named Signal Layer taps and outlets,
- target-specific functions exposed by Nodes where explicitly available.

## Nodes, roles, and Capabilities

- An **Orchestrator** participates in coordination and authority.
- An **Executor** runs Cells and contributes Capabilities without voting.

OS-based and MCU-based Nodes share the Cell model, but they do not expose identical Capabilities. A Cell declares capability tags, and placement selects a compatible Node.

## What ships in the preview

- stable SRI addressing,
- WebAssembly Cells on Linux and supported MCUs,
- Commands, Events, callbacks, and mailbox-based delivery,
- placement by capability tags,
- Cell state and mailboxes held as one copy by default,
- WebSocket and CLI access,
- the native Signal Layer on both Node classes,
- explicit remove-and-report behaviour after Node loss.

The [Guarantees](../08_guarantees.md) page is authoritative for the current release.

## What Myrmic is not

Myrmic is not:

- only an IoT platform,
- merely a WebAssembly runner,
- a generic stateless microservices framework,
- a system that requires a cloud control plane,
- a PLC, hard-real-time controller, or functional-safety system,
- an AI model or inference platform.

## See also

- [Modelling Applications](./03_modelling-applications.md) - how to think in Cells when designing an application
- [Architecture](../07_architecture.md) - how Myrmic actually works, view by view
