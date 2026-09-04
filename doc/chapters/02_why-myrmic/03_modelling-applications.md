# Modelling Applications

Myrmic applications are built from **Cells**: small, isolated units of logic, state, and identity.

A Cell represents something that persists, such as a room, workflow, agent, device, machine, tenant, or asset. It does not represent the temporary process serving it.

> **The interaction is temporary. The Cell is the stable application concept.**

## Applications define the reasoning boundary

An **Application** is the declared boundary for a set of Cell classes, instances, bridges, assets, and deployment settings.

The boundary is organizational, not a guarantee. Cross-Application dependencies are not automatically inferred or managed.

## Cells combine identity, logic, and state

A Cell Class is a WebAssembly module plus its declared requirements. A Cell instance has its own State and Stable Resource Identifier (SRI).

An Application can assign a human-readable Stable Resource Name (SRN). Myrmic derives the SRI deterministically, so any participant that knows the SRN can address the same logical Cell without a location registry.

## Commands, Events, and callbacks

- A **Command** directs intent to one Cell.
- An **Event** publishes a fact to any interested subscribers.
- A **callback** is an ordinary message delivered later to the caller's own mailbox.

Cell-to-Cell work is asynchronous: a Cell dispatches and continues. External clients, including the CLI and gateway, may wait for the later callback and present that wait as a blocking request. Cells themselves never block on another Cell.

## Mailboxes decouple messages from live connections

Messages pass through named outbound and inbound mailboxes synchronized by the Data Layer. This lets a Cell receive work without requiring sender and receiver to be live at the same instant.

The exact durability and delivery contract is versioned on [What Myrmic guarantees today](../08_guarantees.md).

## Handlers define state transitions

A Cell processes one ordered stream of work, one handler at a time.

Conceptually, a command handler groups:

- consuming the inbound message,
- changing the Cell's state,
- producing outbound messages.

External effects such as HTTP calls, MQTT publishes, or actuator writes sit outside that unit and need their own idempotency or fencing strategy.

## Capabilities guide placement today

The current Application specification declares capability tags, build platforms, an optional SRI, and initialization arguments.

A Node advertises what it can provide. Myrmic places the Cell only where its requirements are available.

## Recovery models arrive with Partition Tolerance

The preview does not expose a per-Cell recovery or consistency model.

The **Partition Tolerance** stage introduces three declarations:

- **Consistent** - consistency first; may halt instead of diverging,
- **Convergent** - designed to reconcile divergent state under a declared merge rule,
- **Dataflow** - best-effort stream processing at a declared level.

Restart policy is a separate concern from replication. See [Recovery Models](../06_concepts/06_recovery-models.md) and the [Roadmap](../09_roadmap.md).

## Cell patterns provide design vocabulary

Asset, Adapter, Agent, and Bridge are application patterns, not runtime types or guarantees.

They help teams separate canonical state, physical integration, decision logic, and external connectivity.

## See also

- [Core Concepts](../06_concepts.md) - the full Cell, Mailbox, and Node vocabulary
- [Connecting Physical Systems](./04_connecting-physical-systems.md) - how Cells relate to native drivers and hardware I/O
