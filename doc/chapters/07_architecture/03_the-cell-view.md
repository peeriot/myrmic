# The Cell View

The Cell view answers:

> **What happens when my Cell receives a Message?**

![A step-by-step view of message processing inside one Myrmic Cell](../../images/cell-view.svg)

## 1. A Message waits in the Mailbox

Commands, Events, callbacks, and timer wake-ups enter the Cell through its Mailbox.

The Mailbox separates Message arrival from the moment the Cell is ready to process it.

## 2. One Handler runs

The Cell processes one Message at a time.

The Handler reads the Cell's State, applies Application logic, and stages any Messages it wants to send. A slow Handler delays the work behind it, so Handlers should remain short.

## 3. The Handler stages one unit of work

For a Command Handler, that unit groups:

- consuming the inbound Message,
- updating the Cell's State,
- writing outbound Messages.

The current transaction boundary, including what happens if the Handler fails or the destination lives on another Node, is stated on the [Guarantees](../08_guarantees.md) page.

## Messages continue the workflow

An outbound Message enters another Cell's Mailbox. A result returns as a callback Message to the caller's Mailbox rather than as a blocking Cell-to-Cell call.

## External effects sit outside

HTTP calls, MQTT publishes, and actuator writes sit outside the Cell transaction. Applications must make these effects idempotent, use a sink that supports deduplication or fencing, or accept at-least-once behaviour.

## Two design rules

- Make every Handler safe to run twice.
- Rely on order inside one Cell, never on a global order across Cells.

## See also

- [Messages and Mailboxes](../06_concepts/03_messages-and-mailboxes.md) - how Commands and Events reach a Cell
- [Handlers and Transactions](../06_concepts/04_handlers-state-and-transactions.md) - what runs when a Message is consumed
