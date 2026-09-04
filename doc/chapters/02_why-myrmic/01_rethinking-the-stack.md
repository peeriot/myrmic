# Rethinking the Stack

Software is becoming part of the world around us, inside products, machines, buildings, local infrastructure, and the operations people depend on.

The application stack has not caught up. Teams still combine separate systems for execution, messaging, state, placement, failure handling, hardware integration, and device-specific tooling before they can build the application itself.

> **Software is moving closer to the world it serves. Its infrastructure should move with it.**

## Software is moving closer

Applications increasingly span:

- central services and global users,
- local sites and operational networks,
- gateways and Linux-class systems,
- bare-metal microcontrollers,
- sensors, actuators, and physical processes.

Location now affects latency, continuity, data movement, hardware access, and who remains in control when connectivity changes.

## The stack is fragmented

A stateful application often requires teams to assemble:

- a runtime,
- messaging and stable addressing,
- durable state and mailboxes,
- placement and discovery,
- lifecycle and failure handling,
- gateways and external interfaces,
- native drivers and signal processing,
- different stacks for servers, gateways, and MCUs.

The burden is not one component. It is making all of them behave as one system, then repeating that work for every product.

## Physical entities should participate

Products, machines, rooms, and assets are often reduced to telemetry streams or database rows. Their identity and behaviour are reconstructed somewhere else.

We believe physical entities should participate directly in software systems. They should have stable identities, receive requests, publish events, expose local functions, and support useful local behaviour.

That does not require every state byte to live on the device. It requires the software model to respect where data, hardware, and operational context actually exist.

## Edge-first, not edge-only

**Edge-first** means designing from the real context of the application:

- where data is created,
- where people and devices interact,
- which Capabilities are local,
- what must continue during network disruption,
- where teams want control to remain.

A Myrmic **swarm** is a network of devices and processes that runs Applications as one system. It can form and operate without a cloud connection. An Application that belongs entirely at the edge can run entirely at the edge.

Central and cloud infrastructure remain useful for global access, aggregation, analytics, and cross-site coordination. They are optional deployment choices, not mandatory control paths for every interaction.

## Resilience should become a runtime property

Stateful recovery becomes difficult when identity, authority, and data are fused with one process or machine.

Myrmic separates three concerns:

- **authority:** who decides,
- **data:** what survives,
- **execution:** what currently runs.

The developer preview makes its current boundary explicit. The roadmap then makes authority survive failures with **Partition Tolerance** and data with **Durability**.

## Software should reduce routine operations

Distributed software should organize, coordinate, and recover itself during routine operation without depending on a third-party operator to keep it running.

This does not remove human oversight. Teams still define policy, security, lifecycle, and the boundaries within which the system may act.

## Infrastructure should remain controllable

Myrmic is open source and self-hostable. The runtime, placement model, failure behaviour, and hardware integration remain inspectable and adaptable.

The goal is not to reject cloud services. It is to preserve a credible alternative to making one provider part of every application's control path.

## Developer Preview

Myrmic is available as a developer preview. Use it to evaluate the Cell model, messaging, placement, mixed Linux and MCU operation, the Signal Layer, and the current documented limits.

## See also

- [Understanding Myrmic](./02_understanding-myrmic.md) - the model that responds to this shift
- [Guarantees](../08_guarantees.md) - what the current release promises, and what it doesn't yet
- [Roadmap](../09_roadmap.md) - what future stages are intended to add
