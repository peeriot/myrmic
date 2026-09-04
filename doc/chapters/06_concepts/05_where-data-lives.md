# Where Data Lives

Myrmic separates three data categories that are easy to confuse.

| Data category | What it contains | Current placement policy |
| --- | --- | --- |
| **Platform metadata** | System and swarm records | Available on every Node |
| **Cell classes** | Content-addressed WebAssembly binaries | Available on Linux Nodes |
| **Cell data** | Cell state, mailboxes, and Application records | One copy by default; scope replication can be configured explicitly |

The [Guarantees](../08_guarantees.md) page is authoritative for the current release.

## Cell data is not execution

A Cell may execute on one Node while its State or Mailbox lives on another.

This is intentional. Execution, authority, and data are separate concerns.

Losing the execution Node and losing the Node that holds the data are therefore different failures.

## Authority replication is not data replication

Partition Tolerance replicates authority. It preserves the decisions about who leads a Cell and where it may serve.

Partition Tolerance does not replicate the Cell's State. The authority decision can survive even while the Cell's data still exists as one copy.

Durability is the roadmap stage that replicates a Cell's State and Mailboxes across several Nodes, so that the Cell's data survives the loss of one of them.

## Default does not mean universal

A **Swarm Admin** can configure replication for a data scope. That is an operational choice, not a blanket promise made by the preview.

Design against the versioned guarantee for the actual deployment.

## See also

- [Failure Behaviour](../07_architecture/04_failure-behaviour.md) - what happens when a Node is lost
- [Recovery Models](./06_recovery-models.md) - what happens to a Cell after a restart or Node loss
- [Roadmap](../09_roadmap.md) - what future stages are intended to add
