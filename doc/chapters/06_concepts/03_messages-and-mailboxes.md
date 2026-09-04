# Messages and Mailboxes

Cells communicate through Messages.

## Commands and Events

### Command

A **Command** is a directed request to one Cell, addressed by SRI. It expresses intent and may be rejected.

### Event

An **Event** is a published fact. Every subscriber reads it independently. One subscriber does not consume it for another.

Use a Command when one specific Cell must act. Use an Event when something happened and any interested Cell should know.

## Cells never block on another Cell

There is no synchronous Cell-to-Cell call.

A Cell dispatches a Command and continues. If it requests a result, that result arrives later as a **callback**, which is an ordinary Message in the caller's own mailbox.

External clients can choose to wait. The CLI or gateway may wait for the callback and present a blocking request to the external caller, but the underlying Cell interaction remains asynchronous.

## Delivery tiers

A delivery tier states whether the runtime keeps trying to deliver a Message:

- **tracked:** pursued until it reaches the destination Cell's current mailbox, without a time bound,
- **best-effort:** tried once and not tracked.

The delivery tier is independent of whether an external client waits for a result.

## The mailbox pair

An inbound Message is recorded before the Cell handles it. The current durability boundary is stated on the [Guarantees](../08_guarantees.md) page.

A sender writes to a named outbound mailbox on its Node. The Data Layer synchronizes that mailbox with the named inbound mailbox on the destination side.

The sender does not directly mutate the receiver's inbox.

This distinction matters because atomic delivery across Nodes is a data-replication problem, not only a local transaction problem.

## External clients use the same application path

CLI Commands, gateway requests, and Cell-to-Cell Commands all reach the same addressed Cell model.

A Cell should not need different domain logic for each transport surface.

The current contract for delivery, duplicates, timeouts, and mailbox durability is versioned on [What Myrmic guarantees today](../08_guarantees.md).

## See also

- [Handlers, State, and Transactions](./04_handlers-state-and-transactions.md) - what runs when a Message is consumed
- [Core Concepts](../06_concepts.md) - the full list of concepts this page belongs to
