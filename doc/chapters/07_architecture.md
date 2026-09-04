# Architecture

New to the terminology? Start with [Core Concepts](./06_concepts.md).

Myrmic's architecture is explained through four views. Each answers one question instead of trying to show the whole system at once.

| View | Question | Primary audience |
| --- | --- | --- |
| **Swarm** | Where does my code run? | Everyone |
| **Node** | What runs on a device? | Swarm Admins and contributors |
| **Cell** | What does my code experience? | Application developers |
| **Failure** | What happens when a Node is lost? | Evaluators and Swarm Admins |

A separate stages picture answers: **What arrives when?**

## The structural commitment

Myrmic separates three concerns that many systems fuse:

- **Authority:** who decides leadership, membership, placement, and the serving role.
- **Data:** what survives, including State, Mailboxes, and records.
- **Execution:** what currently runs, with one active WebAssembly guest for each Cell.

Each concern has its own durability and replication story.

> **A Cell becomes sovereign over its Node only when both authority and committed data are replicated.**

That is why the roadmap treats them as two distinct properties: Partition Tolerance for authority and Durability for data.

## One question per diagram

The Swarm, Node, and Cell views explain the current architecture without mixing in roadmap claims. Failure behaviour is shown separately for the developer preview, Partition Tolerance, and Durability.

Roadmap diagrams use amber, dashed elements and include the stage name inside the image. Current-state diagrams use solid elements.

## The views

1. [The Swarm View](./07_architecture/01_the-swarm-view.md)
2. [The Node View](./07_architecture/02_the-node-view.md)
3. [The Cell View](./07_architecture/03_the-cell-view.md)
4. [Failure Behaviour](./07_architecture/04_failure-behaviour.md)
5. [The Six Layers](./07_architecture/05_the-six-layers.md)

The [Guarantees](./08_guarantees.md) page, not a diagram, is authoritative for current release behaviour.
