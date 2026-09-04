# Nodes, Capabilities, and the Swarm

A **Node** is a device or operating-system process running Myrmic. It executes Cells, exposes local Capabilities, and connects to the swarm.

A **swarm** is the set of Nodes that have found each other and behave as one system.

Cells address each other by a Stable Resource Identifier (SRI), without knowing which Node currently runs them.

## Two Node shapes

### OS-based Node

A Linux-class Node can run plugins for execution, self-organization, and data services, along with gateways and the native Signal Layer.

### MCU-based Node

A supported MCU-based Node uses WAMR to execute WebAssembly modules that were compiled ahead of time. It also runs typed platform clients, `zenoh-nano`, and native Signal Layer tasks.

The Node shape determines the available Capabilities, not what a Cell means.

## Two Node roles

### Orchestrator

Participates in coordination and authority: membership, leadership, and placement.

### Executor

Runs Cells and contributes Capabilities without voting.

Roles and shapes are distinct. In practice, constrained MCUs act as Executors.

## Swarm Admin

A **Swarm Admin** is the person responsible for configuring and monitoring a swarm. This is a user role, not a Node role.

## Capabilities and placement

A Node advertises what it offers:

- architecture and build platform,
- protocols, radios, and device interfaces,
- native modules,
- Signal Layer taps and outlets,
- gateway or data roles.

A Cell declares capability tags. Placement matches the two.

Capabilities are the basis of placement and the honest limit of portability. The Cell model spans device classes, but Myrmic does not place a Cell where its required functions are unavailable.

## What Cells declare today

The developer preview supports capability tags, build platforms, an optional SRI, and initialization arguments.

It does **not** yet expose a per-Cell recovery or consistency model. Those declarations arrive with Partition Tolerance.

## See also

- [Recovery Models](./06_recovery-models.md) - what happens to a Cell after a restart or Node loss
- [The Swarm View](../07_architecture/01_the-swarm-view.md) - where code runs, in diagram form
