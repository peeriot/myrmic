# Recovery Models

> **Roadmap concept, introduced with Partition Tolerance**  
> The current developer preview does not expose a recovery or consistency model for each Cell. Today, Cells declare capability tags. Recovery models are the future declaration axis for authority and recovery behaviour.

A recovery model answers how a Cell should behave when the swarm is partitioned or its serving Node disappears.

## Consistent

**Consistent** declares that the Cell must not diverge. It may become unavailable when a majority of its voter set cannot be reached.

Its additional configuration includes:

- an odd replica band,
- an action to take after failure.

A Consistent Cell can range from one unreplicated instance to a replicated deployment. Replicated data arrives later with Durability.

## Convergent

**Convergent** declares that segments may diverge and later reconcile under a defined merge rule.

Availability and merge behaviour are not current guarantees. A defensible merge model belongs to the **Secure Swarm** roadmap stage and must be demonstrated before it is claimed.

## Dataflow

**Dataflow** declares best-effort stream processing at a stated level. It uses neither replicated authority nor replicated State.

## Restart policy is separate

A restart policy answers what happens after a process restart, reboot, or site-wide power loss.

That is different from deciding whether authority and data are replicated. The two settings must remain separate.

## Orthogonal concerns

A recovery model does not replace:

- **capability tags**, which decide where a Cell may run,
- **execution shape**, such as continuous stream processing,
- **Cell patterns**, which describe the role a Cell plays in the Application.

When recovery models arrive, Myrmic will refuse a replicated stream deployment rather than silently degrade it.

## Sovereign Cells

A **Sovereign Cell** is a Cell that outlives the Node it runs on. A Consistent Cell becomes sovereign when it has both:

- replicated authority from **Partition Tolerance**,
- replicated committed data from **Durability**.

> **Partition Tolerance decides who may serve. Durability preserves what the Cell knows.**

## See also

- [Roadmap](../09_roadmap.md) - what future stages are intended to add
- [Guarantees](../08_guarantees.md) - what the current release promises, and what it doesn't yet
- [Cell Patterns](./07_cell-patterns.md) - recurring designs for combining Cells, Adapters, Agents, and Bridges
