---
sidebar_label: Platform Engineering
---

# Give developers one stateful application primitive

Platform teams often standardize deployment while each application team still invents its own combination of state, messaging, scheduling, placement, and recovery.

## The Myrmic approach

Offer the Cell as the shared primitive for stateful applications.

Application teams define state, handlers, Commands, Events, timers, Application boundaries, and capability tags. The platform team operates Nodes, data roles, orchestration, gateways, and policy.

## What the preview demonstrates

- one stateful programming model,
- stable logical addressing,
- mailbox-based messaging,
- placement by capability tags,
- OS-based and MCU-based Nodes in one swarm,
- explicit documentation of what happens after Node loss,
- an inspectable open-source stack.

## What the roadmap adds

- per-Cell recovery models,
- replicated authority,
- fencing that permits one active serving copy,
- replicated Cell State and Mailboxes,
- Cell resume after Node loss,
- request identity and durable timers.

## Start an evaluation

1. Choose one repeated stateful pattern.
2. Implement it as a Cell.
3. Define its capability tags.
4. Reproduce the documented behaviour for Messages, State, and Node loss.
5. Decide whether Partition Tolerance, Durability, and Secure Swarm close a real platform gap.

> **Standardize the application model, not only the deployment pipeline.**

## See also

- [Target Groups](../03_target-groups.md) - the other entry points Myrmic supports
- [Concepts](../06_concepts.md) - the vocabulary every other page assumes
- [Guarantees](../08_guarantees.md) - what the current release promises, and what it doesn't yet
