# Operating Myrmic

A **swarm** is a network of Myrmic Nodes that behaves as one system. Myrmic lets different device classes participate while keeping their roles and limits explicit.

The developer preview is intended for evaluation. Operate it against the published guarantee contract, not against assumptions about future continuity.

## Nodes have shapes and roles

A **Node** is a device or operating-system process running Myrmic.

Node shapes:

- **OS-based:** Linux-class systems with the plugin topology, broader compute, gateways, and data services.
- **MCU-based:** supported microcontrollers with firmware execution, WAMR with modules compiled ahead of time, typed clients, and native Signal Layer tasks.

Node roles:

- **Orchestrator:** participates in coordination and authority.
- **Executor:** runs Cells and contributes Capabilities without voting.

MCU-based Nodes operate as Executors.

## Placement uses Capabilities

Nodes advertise Capabilities such as architecture, protocols, storage roles, native modules, and Signal Layer taps or outlets.

Cells declare capability tags. The current placement logic selects a reachable execution Node that provides the required Capabilities.

Recovery models are not part of the preview Application manifest. They arrive with Partition Tolerance.

## Authority, data, and execution are separate

A Cell may execute on one Node while its State and Mailbox live on another.

Today:

- authority uses one coordinator selected by lowest ID,
- Cell data is held as one copy by default,
- one Cell instance serves, but redeployment does not yet fully prevent a brief overlap.

The [Guarantees](../08_guarantees.md) page states the exact current contract.

## Current behaviour after Node loss

When a Node is lost, Myrmic removes and reports affected deployments. It does not claim that the Cell resumes elsewhere.

Without a State replica, there is nothing to resume from. Starting a fresh, empty Cell under the same identity would not be continuity.

## Messaging and external access

Cells communicate through Commands, Events, and callbacks over Mailboxes.

External clients can interact through:

- the CLI,
- the WebSocket gateway,
- packaged web applications,
- other integrations exposed by Nodes.

The Chatty example demonstrates browser sessions, history, presence, and message fan-out through the same Cell model.

## Common deployment shapes

### Central swarm

OS-based Nodes provide execution, data, gateways, and coordination.

### Site-local swarm

Applications run close to users, assets, and local networks without requiring every interaction to cross a cloud path.

### Hybrid swarm

Local Nodes handle context-sensitive work while central Nodes provide broader services.

### MCU plus Linux

Bare-metal Executors provide local Cells and Signal Layer Capabilities. Linux Nodes provide data, orchestration, and gateways.

## Development workflow

```bash
myrmic new my-cell
myrmic build ./my-cell
myrmic runtimes start --detached
myrmic deploy ./my-cell
myrmic send my-cell increment
myrmic telemetry debug --sri my-cell
```

> **Create · Build · Run · Deploy · Interact · Observe**

## What the roadmap changes

- **Partition Tolerance:** authority over each Cell is held by a voter set of several Nodes and decided by majority, and fencing allows only one active serving copy. Deploy and stop operations become acknowledged.
- **Durability:** Cell State and Mailboxes are replicated across Nodes. Transactions can extend across Nodes, timers become durable, Commands gain request identity, and a Cell can resume after Node loss.
- **Secure Swarm:** Nodes and Cells become authenticated, Nodes can join and leave while Cells keep running, and upgrades get defined semantics.

Durability is research before engineering. The roadmap names the target guarantee and the demonstration that will prove it, without pretending the full design is already scheduled.

## Evaluate the preview actively

- reproduce the current Message and transaction behaviour,
- test placement by capability tags on your target Nodes,
- stop the Node holding Cell data and observe the documented result,
- compare the diagrams with the guarantee page,
- report any behaviour that contradicts the contract.

## See also

- [Architecture](../07_architecture.md) - how Myrmic actually works, view by view
- [Guarantees](../08_guarantees.md) - what the current release promises, and what it doesn't yet
- [Roadmap](../09_roadmap.md) - what future stages are intended to add
