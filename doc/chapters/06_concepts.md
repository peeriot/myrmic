---
sidebar_label: Core Concepts
---

# Core Concepts

Eleven concepts carry the Myrmic model. Everything else is detail beneath one of them.

| Concept | One-line definition |
| --- | --- |
| **Application** | The boundary a developer declares and reasons about |
| **Cell** | The unit of logic, state, and identity |
| **SRN** | A human-readable, hierarchical name chosen for a Cell |
| **SRI** | The stable identifier Myrmic derives from an SRN and uses to address the Cell |
| **Node** | A device or operating-system process running Myrmic |
| **Swarm** | Nodes that have found each other and behave as one system |
| **Capability** | A function or resource a Node offers and a Cell requires for placement |
| **Message** | A directed Command, a published Event, or a later callback |
| **Mailbox** | The named storage path every Message uses |
| **Handler** | The Cell's reaction to one Message and its state-transition boundary |
| **State** | The Cell's owned memory between interactions |

A developer writes the SRN. Myrmic derives the SRI deterministically, so the same SRN produces the same SRI on every Node without a naming registry.

```text
SRN: product-order-021:item-011
SRI: 018f3c2e-9d41-7a52-b3ae-5f0c19c47d11
```

The model fits together in a natural order:

> **An Application declares Cells. Each Cell is named by an SRN and addressed by its stable SRI. It runs on a Node inside a swarm, is placed where its required Capabilities exist, receives Messages through its Mailbox, reacts in a Handler, and remembers in its State.**

## Read the model in order

1. [Applications, Cells, and Identity](./06_concepts/01_applications-cells-and-identity.md)
2. [Nodes, Capabilities, and the Swarm](./06_concepts/02_nodes-capabilities-and-the-swarm.md)
3. [Messages and Mailboxes](./06_concepts/03_messages-and-mailboxes.md)
4. [Handlers, State, and Transactions](./06_concepts/04_handlers-state-and-transactions.md)
5. [Where Data Lives](./06_concepts/05_where-data-lives.md)
6. [Recovery Models](./06_concepts/06_recovery-models.md)
7. [Cell Patterns](./06_concepts/07_cell-patterns.md)

Concept pages explain what things are. [What Myrmic guarantees today](./08_guarantees.md) states what the current release promises. The [Roadmap](./09_roadmap.md) states what future stages are intended to add.
