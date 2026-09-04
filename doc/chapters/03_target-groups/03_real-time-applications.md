---
sidebar_label: Real-Time Applications
---

# Give every room, session, and match a durable home

Rooms, sessions, and matches outlive the processes serving their current connections.

Teams often split identity and state across WebSocket servers, caches, databases, brokers, and recovery jobs.

## The Myrmic approach

Model each long-lived real-time concept as a Cell.

```text
browser clients
      ↓
WebSocket gateway
      ↓
room or match Cell
state · Commands · Events · timers
```

The Cell owns membership, history, rules, and behaviour. Browser clients use the same Commands and Events as other Cells.

## What the preview demonstrates

- stable Cell identity,
- mailbox-based messaging,
- one ordered stream of Handler invocations within each Cell,
- dynamic Cell creation,
- WebSocket access and packaged web assets,
- Cell State held as one copy by default,
- explicit behaviour after Node loss rather than hidden recovery assumptions.

The Chatty example demonstrates sessions, history, presence, and message fan-out without any Signal Layer dependency.

## What the roadmap adds

Partition Tolerance uses fencing to allow one active serving role and adds acknowledged lifecycle operations. Durability replicates State and Mailboxes across Nodes, so Consistent rooms or matches can survive Node loss. Durability also adds request identity and durable timers.

> **Connections are temporary. The room is not.**

## See also

- [Target Groups](../03_target-groups.md) - the other entry points Myrmic supports
- [Modelling Applications](../02_why-myrmic/03_modelling-applications.md) - how to think in Cells when designing an application
- [Guarantees](../08_guarantees.md) - what the current release promises, and what it doesn't yet
