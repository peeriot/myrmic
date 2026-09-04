---
sidebar_label: Workflows and Agents
---

# Give long-running work a place to live

Workflows and agents need state that survives individual requests and workers.

Today that state is often spread across queues, scheduler entries, database rows, callback correlation, and retry code.

## The Myrmic approach

Represent each workflow or agent as a Cell.

The Cell keeps context, receives Commands and Events, schedules continuation, and can create child Cells for delegated work. Results return as callbacks into the caller's mailbox.

## What the preview demonstrates

- stable identity and Cell State,
- one ordered stream of work,
- callbacks and process-local timers,
- placement by capability tags,
- dynamic Cell creation and optional lifetime leases,
- explicit timeout and retry semantics.

## What the roadmap adds

- Partition Tolerance introduces the Consistent recovery model and fencing for one active serving copy.
- Durability adds replicated State, durable timers, request identity, deduplication, and Cell resume after Node loss.

## Boundary

Myrmic does not provide an AI model or inference engine. It provides state, messaging, placement, and lifecycle around an agent.

## See also

- [Target Groups](../03_target-groups.md) - the other entry points Myrmic supports
- [Messages and Mailboxes](../06_concepts/03_messages-and-mailboxes.md) - how Commands and Events reach a Cell
- [Roadmap](../09_roadmap.md) - what future stages are intended to add
