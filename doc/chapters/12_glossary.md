# Glossary
Entries are listed alphabetically.

| Term | Definition |
|---|---|
| Adapter Cell | Bridges hardware or device protocols into Commands and Events. Adapter Cells are bound to Hosts with the required physical access. |
| Agent Cell | Implements application-specific behavior on top of Assets and Adapters. Agent Cells observe, decide, and act, and are typically relocatable. |
| Asset Cell | Represents a physical or abstract asset and owns its canonical state. Asset Cells are logical and typically relocatable. |
| Bridge Cell | Connects the swarm to external systems such as HTTP or MQTT. Bridge Cells are typically deployment-bound to specific network endpoints. |
| Cell | A message-driven, stateful Wasm module that exposes command and event handlers, owns its private persistent state, and is loaded dynamically onto an execution runtime in response to a load request. The smallest unit of compute, state, and identity in Myrmic. |
| Cell Class | A blueprint associated with a WASM binary used to instantiate multiple Cells. |
| Cell Collective | A group of Asset Cells that behaves as a higher-level entity. There is no dedicated Collective type; it is modeled by parent Asset Cells and their Commands and Events. |
| Cell Instance | A running instance created from a Cell Class with its own SRI and state. |
| Command | A directed message that asks a specific Cell to perform an operation or change state. Commands express intent and may be rejected. |
| Command Response | A reply to a Command that carries a result, failure, or acknowledgement. |
| Data Layer | The shared persistence layer that stores per-Cell state in private scopes and provides transactional durability. It also backs the Cell Mailbox. |
| Event | A message that represents a fact that already happened and can be observed by multiple Cells. |
| Fuel metering | A compute budget mechanism that limits how many Wasm instructions a module can execute before yielding. |
| Host | The runtime process on a device that executes Cells, manages their mailboxes, and provides access to capabilities and persistence. |
| Host interface | The set of named import modules that the platform exposes to Wasm modules at the sandbox boundary. |
| Mailbox | The per-cell delivery mechanism for inbound commands and events, realised as dedicated tables in the Data Layer that are polled by a per-cell listener inside the execution runtime. |
| Message | The unit of communication between Cells. Message types include Commands, Events, and Command Responses. |
| Self-Organization Layer (SOL) | The decentralized control plane responsible for discovery, placement, routing, health, and failover across Hosts. |
| SRI | Stable Resource Identifier. The logical identity used to address a Cell regardless of placement, derived deterministically from its SRN. |
| SRN | Stable Resource Name. The readable, stable name given to a Cell, such as `application/worker`, from which its SRI is derived. |
| zenoh-nano | A custom `no_std` Rust implementation of the Zenoh protocol for embedded devices. |
