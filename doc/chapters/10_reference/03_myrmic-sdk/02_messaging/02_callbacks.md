# Callbacks

> **Availability:** Linux and embedded runtimes

A command returns nothing to its sender. A callback is a reference to one of the sender's own command handlers, sent along with the command, so the receiver can call it back with an answer.

The runtime uses callbacks the same way elsewhere: a timer, a Bluetooth scan, and a characteristic read each take one and invoke it when they have something to deliver.

## When to use

Use a callback when the receiver of a command should answer the caller.

Use an event instead for something any interested cell may react to.

## Operations

- Create a callback naming one of your own command handlers.
- Create a callback from a command name, when no handler is in scope.
- Include a callback in a command payload.
- Invoke a callback to answer the caller.

## Example

```rust
use myrmic_sdk::{Callback, Metadata, Sri};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
struct StatusReply {
    active: bool,
}

// On the receiving cell.
#[myrmic_sdk::cmd]
fn get_status(md: Metadata, reply: Callback<StatusReply>) -> myrmic_sdk::Result {
    // Sends a command back to the caller. A second invoke would not compile,
    // because this one consumes the callback.
    reply.invoke(md.sender, &StatusReply { active: true })
}

// On the calling cell.
#[myrmic_sdk::cmd]
fn on_status(_md: Metadata, reply: StatusReply) -> myrmic_sdk::Result {
    myrmic_sdk::info!("active={}", reply.active)?;

    Ok(())
}

// Also on the calling cell.
#[myrmic_sdk::cmd]
fn request_status(_md: Metadata) -> myrmic_sdk::Result {
    // The receiver's identity, derived from its name.
    let receiver = Sri::of_path("myapp/worker").map_err(|_| "not a valid SRN path")?;

    // Callback::of takes the payload type from on_status, so the receiver
    // can only answer with a StatusReply.
    myrmic_sdk::send(receiver, "get_status", &Callback::of::<on_status>())
}
```

## Behavior

### Normal

A callback carries the name of a command handler on the cell that created it. Invoking it sends that command, so an answer is an ordinary command rather than a value returned to the caller.

The original command can finish before the answer arrives. Only a command handler can be named.

Everything a command handler does commits atomically when it completes, including an answer it sent, so nothing is visible outside until then.

### Errors

Creating a callback from a name fails when the name is invalid. Invoking a callback fails when the answer cannot be encoded, when no cell exists at the caller's identity, or when the runtime rejects the request.

An invocation that did not come from a cell carries an empty sender, so there is nothing to answer. Check for it before invoking.

### Limits

A callback carries no request identifier, so a cell with several requests in flight has to put its own in the payload.

## API documentation

For both ways to build a callback and the invoke signature, see [`Callback`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/struct.Callback.html).
