# Work with commands

## What is a command

A command is a directed request sent to a specific cell, triggering an action. It has a name, a handler, and a payload.

This guide puts commands in practice and shows how to work with them.

## Command Handler

A command handler is a Rust function annotated with the `#[cmd]` macro provided by the Myrmic SDK.

```rust
#[myrmic_sdk::cmd]
fn greet(md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("hello from {:?}", md.id).ok();

    Ok(())
}
```

`#[cmd]` exports the annotated function as a Wasm command. During deployment, the runtime discovers all exported commands and registers them for invocation. By default, the command name is the handler function name - it is the name callers use to invoke the command.

To override the command name, pass the `name` attribute to the macro:

```rust
#[myrmic_sdk::cmd(name = "hello")]
fn greet(md: myrmic_sdk::Metadata) -> myrmic_sdk::Result {
    // ...
    Ok(())
}
```

Every command handler takes `Metadata` as its first argument - it carries context about who called it and which cell is running:

```rust
pub struct Metadata {
    pub id: Sri,     // the SRI of this cell
    pub sender: Sri, // the SRI of the sender; itself for self-calls or timer/BLE triggers; nil when sent externally (e.g. CLI)
}
```

## Command payload

A command may require an input payload to fulfil its logic - in that case, add a second argument to the handler function to receive it:

```rust
#[myrmic_sdk::cmd]
fn set_threshold(_md: Metadata, value: f32) -> myrmic_sdk::Result {
    // ...
    Ok(())
}
```

The payload type must implement `Decoder` and `Encoder` - traits the SDK uses to deserialize and serialize the payload. Both rely on [`serde::Serialize`](https://docs.rs/serde/latest/serde/trait.Serialize.html) and [`serde::Deserialize`](https://docs.rs/serde/latest/serde/trait.Deserialize.html), which must also be derived. To understand why and how, see [Message encoding](./04_message-encoding.md).

Numeric primitives (`f32`, `u32`, `i32`, `bool` ...), and types provided by the SDK such as `myrmic_sdk::String`, `myrmic_sdk::Bytes`, and `myrmic_sdk::JsonValue` already implement it by default.

A command is limited to one payload parameter - if you need multiple fields, wrap them in a struct and derive `myrmic_sdk::Message` to automatically implement `Decoder` and `Encoder`:

```rust
use serde::Deserialize;

#[derive(Deserialize, myrmic_sdk::Message)]
struct WorkItem {
    id: myrmic_sdk::String,
    priority: u32,
}

#[myrmic_sdk::cmd]
fn process(_md: myrmic_sdk::Metadata, item: WorkItem) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("processing {} (priority {})", item.id, item.priority).ok();

    Ok(())
}
```

## Invoke a command from the CLI

Rather than writing a second cell just to test, the CLI provides [`myrmic send`](../10_reference/02_myrmic-cli/08_send.md) - a command (CLI) that targets a running cell directly from a terminal.

It takes the cell SRI, a command name, and an optional payload, and dispatches the command to the target cell:

```bash
# No payload
myrmic send my-cell greet

# JSON payload
myrmic send my-cell process '{"id":"item-01","priority":2}'
```

The payload is sent as JSON by default. For a full reference see [`myrmic send`](../10_reference/02_myrmic-cli/08_send.md).

## Invoke a command from one cell to another

Now we reach the interesting part - a cell sending commands to another cell.

```rust
fn forward() -> myrmic_sdk::Result {
    let target = myrmic_sdk::Sri::of_path("worker").map_err(|_| "invalid sri")?;
    let payload = WorkItem {
        id: myrmic_sdk::String::from("item-01"),
        priority: 2,
    };

    myrmic_sdk::send(target, "process", &payload)?;

    Ok(())
}
```

The Myrmic SDK provides the `send` function to invoke commands. It takes the target SRI, the command name, and the payload.

`send` is callable from any handler within a cell - a command handler, an event handler, or an initialization handler. A cell can also invoke commands on itself.

For commands that take no payload, the SDK provides `myrmic_sdk::Void` - a zero-sized placeholder that signals no data is being sent:

```rust
myrmic_sdk::send(target, "greet", &myrmic_sdk::Void)?;
```

## Dispatch from a command handler

`send` is non-blocking - execution continues immediately after each call. All command sends (and event publishes) made during a command handler commit atomically together with any storage operations when the handler completes - nothing is visible to the outside until that point.

## Register a callback to receive a response from a command

Commands are fire-and-forget - once dispatched,  the execution continues immediately and there is no way to get a value back directly.

If a command is expected to produce a result, the solution is to include a callback in the payload. The callee receives it, does its work, and sends the result back through it. On the caller side, the result lands on a dedicated command handler.

```rust
use serde::{Deserialize, Serialize};


// Shared type - both sides must use the same definition
#[derive(Serialize, Deserialize, myrmic_sdk::Message)]
struct StatusReply {
    active: bool,
    label: myrmic_sdk::String,
}
```

The caller sends the callback and handles the result in a dedicated handler:

```rust
#[myrmic_sdk::cmd]
fn request_status(_md: Metadata) -> myrmic_sdk::Result {
    let target = myrmic_sdk::Sri::of_path("worker").map_err(|_| "invalid sri")?;
    let callback = myrmic_sdk::Callback::of::<on_status>(); // or: Callback::to("on_status")?

    myrmic_sdk::send(target, "get_status", &callback)?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_status(_md: Metadata, reply: StatusReply) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("active={} label={}", reply.active, reply.label).ok();

    Ok(())
}
```

The callee receives the callback and invokes it with the result:

```rust
#[myrmic_sdk::cmd]
fn get_status(md: myrmic_sdk::Metadata, callback: myrmic_sdk::Callback<StatusReply>) -> myrmic_sdk::Result {
    let reply = StatusReply {
        active: true,
        label: myrmic_sdk::String::from("worker-01"),
    };

    callback.invoke(md.sender, &reply)?;

    Ok(())
}
```

`Callback<T>` is a type that holds a reference to a handler on the calling cell meant to handle the response. `T` is the reply type that handler accepts.

`Callback` provides two ways to create an instance:

- `Callback::to("on_status")` - takes the name of the handler to invoke on the caller.
- `Callback::of::<on_status>()` - uses the handler's marker type; the compiler verifies it accepts `T`.

## See also

- [How to publish and handle events](./03_events.md)
- [Scheduling Handlers](./05_scheduling-handlers.md) - how to schedule work at a fixed interval or after a delay
- [Message encoding](./04_message-encoding.md)
- [State and Storage](./06_state-and-storage.md) - working with state and storage
- [`myrmic send` reference](../10_reference/02_myrmic-cli/08_send.md)

## Related SDK reference

- [Commands](../10_reference/03_myrmic-sdk/02_messaging/01_commands.md)
- [Callbacks](../10_reference/03_myrmic-sdk/02_messaging/02_callbacks.md)
- [Message encoding](../10_reference/03_myrmic-sdk/02_messaging/04_message-encoding.md)
