---
sidebar_label: Asset-Centric Software
---

# Make every asset a first-class application participant

Physical assets exist continuously. Their software representation often does not.

State is split across telemetry, gateways, database records, rule engines, and remote services. The application must reconstruct the asset from scattered streams and records.

## The Myrmic approach

Model each asset as a Cell with stable identity and State, while keeping the physical boundary explicit.

```text
sensor or actuator
        ↓
native Signal Layer
        ↓
Adapter Cell placed by capability tags
        ↓ Commands and Events
Asset Cell
```

The Signal Layer acquires and processes data close to the asset. The Adapter translates local signals into the application model. The Asset Cell owns canonical state, policy, and behaviour.

## What the preview demonstrates

- stable identity for the asset,
- local native signal processing,
- placement by capability tags,
- Cell State and Mailboxes held as one copy by default,
- one interface for other Cells, CLI clients, and web applications,
- explicit delete-and-report behaviour after Node loss.

## What the roadmap adds

Partition Tolerance introduces the **Consistent** recovery model and replicated authority, so the decision about which Node serves an Asset Cell survives Node loss. Durability replicates the Asset Cell's State and Mailboxes, so the Cell itself can resume on an eligible Node with the State it had.

Together, those two properties make the Asset Cell a Sovereign Cell: it outlives the Node it runs on.

## Good fits

- machines and production assets,
- buildings and rooms,
- energy systems,
- technical infrastructure,
- distributed equipment fleets,
- non-safety-critical local automation.

## Boundary

Myrmic is not a certified safety controller. Keep hard-real-time and functional-safety functions in systems designed for them.

## See also

- [Target Groups](../03_target-groups.md) - the other entry points Myrmic supports
- [Connecting Physical Systems](../02_why-myrmic/04_connecting-physical-systems.md) - how Cells relate to native drivers and hardware I/O
- [Recovery Models](../06_concepts/06_recovery-models.md) - what happens to a Cell after a restart or Node loss
