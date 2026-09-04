# Cell Patterns

Cell patterns are design conventions, not runtime types.

They help teams separate canonical state, physical integration, decisions, and external connectivity. Patterns carry no guarantees.

| Pattern | Role in the Application | Typical future recovery model |
| --- | --- | --- |
| **Asset** | Owns canonical state for a machine, zone, room, product, or other thing | Consistent |
| **Adapter** | Translates hardware or a device protocol into Commands and Events | Dataflow; Consistent when it owns durable state |
| **Agent** | Evaluates conditions, coordinates work, and issues Commands | Consistent or Dataflow, depending on state ownership |
| **Bridge** | Connects the swarm to an external server or service over a protocol such as HTTP or MQTT | Convergent by default when recovery models arrive |

Recovery models are introduced with Partition Tolerance. The table is design guidance, not current preview behaviour. Myrmic does not claim Convergent availability during a partition until it has demonstrated a declared merge rule.

## Asset

Own the canonical state and behaviour of a persistent concept. Other Cells query it, command it, or subscribe to its Events.

## Adapter

Translate local signals or protocols into the Cell model. Use capability tags to place the Adapter where the required hardware or native module exists.

## Agent

Hold working context, react to Events, evaluate policy, and issue Commands. Keep canonical Asset state outside the Agent where possible.

## Bridge

Connect Myrmic to an external system. Forward and translate rather than hiding domain decisions inside the integration boundary.

Current HTTP and MQTT Bridges run natively in one designated platform location rather than as ordinary WebAssembly Cell instances. Their integration pattern and future recovery model are separate concerns.

## Composition patterns

Common compositions include:

- Command and Event segregation,
- edge aggregation,
- local decision loops,
- digital twins,
- sagas and long-running coordination.

## See also

- [Applications, Cells, and Identity](./01_applications-cells-and-identity.md) - what a Cell is and how it's addressed
- [Recovery Models](./06_recovery-models.md) - what happens to a Cell after a restart or Node loss
