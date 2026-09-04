---
sidebar_label: Guarantees
---

# What Myrmic Guarantees Today

If Cell, Node, or Mailbox is new to you, start with [Core Concepts](./06_concepts.md).

Failures in distributed systems are easy to describe imprecisely.

This page states what the developer preview promises, so you can build on it. It also states what the preview does **not** promise, so you do not rely on behaviour that is not there.

> **Reproduce anything here that matters to you.** This page should be versioned with every release.

Because the project is still in preview, ambiguous or release-sensitive behaviour is described conservatively.

| Topic | What Myrmic guarantees today | Coming |
| --- | --- | --- |
| **Tracked delivery** | A tracked Message is pursued until it reaches the Mailbox on the Node where the destination Cell is currently running. Delivery is local to that Node and can be revoked: if the Cell is recreated elsewhere, the Message becomes undelivered again and is pursued anew. There is one copy of the Mailbox by default. Losing its Node also loses the Messages it holds. | **Continuity:** the Mailbox is held by a quorum and survives the loss of one Node. |
| **Best-effort delivery** | Tried once. It may never arrive, and the sender is not notified. Addressing a Cell that does not exist produces an immediate error. | - |
| **Duplicates** | Myrmic does not intentionally create another copy after a Message reaches the current Mailbox. A Handler can still see the same logical work twice if a caller sends again, a redeployment overlaps, or a commit does not complete cleanly. Make every Handler safe to run twice. | **Continuity:** request identity and deduplication for supported client Commands. |
| **What commits together** | While a Command Handler runs, one transaction on the Node covers Mailbox consumption, Cell State, and outbound Messages. If the destination Cell's Mailbox lives on another Node, it is outside that local unit. In the current preview, do not assume that every failed Handler has rolled back every part of its work. Write defensively until the release contract confirms the hardened behaviour. | **Continuity:** atomic delivery across Nodes. The committed batch for a Consistent Cell is then held by a quorum. |
| **External effects** | HTTP calls, MQTT publishes, and actuator writes happen outside the Cell transaction. If surrounding work fails, the effect may already have happened. Make effects idempotent or accept at-least-once behaviour. | **Continuity:** effectively-once behaviour only for sinks that support deduplication or fencing. |
| **Ordering** | Inside one Cell, there is one ordered stream and one Handler runs at a time. Across Cells, there is no global order at any stage. | - |
| **State durability** | Cell State is durable across a process restart. By default, the State is held as one copy, so it is not durable if the Node holding it is lost. Platform metadata is available on every Node, and Cell binaries are available on Linux Nodes. A Swarm Admin can configure replication for a scope, but that is not the default guarantee. | **Continuity:** acknowledged writes to a Consistent Cell are held by a quorum. |
| **When a Node is lost** | Affected deployments are removed and reported. Myrmic does not restart the Cell elsewhere or resume it. Without a replica of its State, there is nothing to resume from. Starting an empty Cell under the same identity would not be continuity. | **Quorum:** a surviving majority elects a new leader and serving role. **Continuity:** the Consistent Cell resumes elsewhere from replicated State. |
| **One running copy** | Redeploying a Cell that is already running on a Node can briefly start the new copy before the old one has stopped. Undeploy returns when Myrmic has forgotten the Cell, not when the halt has been acknowledged. | **Quorum:** fencing enforces one active serving copy. Deploy and stop operations are acknowledged. |
| **Waiting and retrying** | Cells do not block on other Cells. They dispatch a Command and receive a later callback. External clients may wait, but a timeout means the outcome is unknown. There is no request identity, so sending again creates a new Command. Timers and Event positions do not survive a Cell restart. | **Continuity:** request identity, deduplication, and durable timers. |
| **Isolation between Cells** | A Cell cannot read or overwrite another Cell's private data. Its scope is derived from its own identity. Messages between Nodes currently carry no cryptographic proof of the sender, so the preview assumes a trusted network. | **Quorum or Beyond:** authenticated principals and an authenticated fabric. |
| **Execution isolation** | Each Cell runs in a WebAssembly sandbox with private memory and only the functions the Node granted it. Memory isolation is strong. Time and throughput isolation are not hard guarantees in the preview. | - |
| **Deployment** | Application deployment is all-or-nothing and rolls back on failure. Unsupported combinations are refused. Tearing down a Cell removes its instance record, so a later deployment creates a new Cell rather than resuming the old one. | **Quorum:** deploy and undeploy become idempotent and acknowledged. |

## Four rules for writing a Cell

1. **Make every Handler safe to run twice.** Idempotency is part of the interface.
2. **The absence of a callback does not mean the Command failed.** Never compensate on that assumption.
3. **Assume one copy of your State** unless your deployment explicitly configures otherwise.
4. **Order what you need yourself.** There is order inside a Cell and none between Cells.

## Read future claims by stage

- **Quorum** changes the guarantees for authority and for which Cell instance may serve. It does not replicate Cell data.
- **Continuity** changes the durability of Cell State and Mailboxes.
- **Beyond** covers trust, reconfiguration, controlled self-healing, upgrades, and declared merge rules.

## Verification

Myrmic's public guarantees are intended to be checked rather than believed.

### Deterministic protocol core

The roadmap architecture uses a deterministic, sans-IO protocol core:

```text
step(input, context) → effects
```

Time and randomness are injected. The same input and seed can reproduce the same protocol decisions.

This supports replayable authority and failure scenarios, but it does not prove faults the harness does not inject.

### Match evidence to the claim

Different properties need different evidence:

- runnable tests for message, handler, transaction, and deployment behaviour,
- seeded scenarios for leadership, fencing, and serving behaviour,
- deliberate loss of the Node that holds the only data copy,
- staged demonstrations for Quorum and Continuity,
- published load tests only when topology, configuration, raw results, and reproduction commands are available.

### Publish mechanisms before numbers

The developer preview does not publish unverified throughput, latency, or recovery figures as facts.

When measurements are published, they should include:

- the exact release,
- hardware and Node topology,
- workload and state size,
- failure injection method,
- configuration and replication settings,
- raw results and reproduction steps,
- known limits.

### A green suite is not the whole contract

Tests can verify only the behaviour they exercise. The versioned guarantee page remains the public contract, and each release should connect every row to an observable check.

### Visual honesty

Architecture diagrams use:

- solid elements for the developer preview,
- amber dashed elements labelled with **Quorum** or **Continuity** for future capability.

A diagram must not make a roadmap element look like current behaviour.

> **Do not just read the claims. Reproduce them.**

## See also

- [Failure Behaviour](./07_architecture/04_failure-behaviour.md) - what happens when a Node is lost
- [Roadmap](./09_roadmap.md) - what future stages are intended to add
