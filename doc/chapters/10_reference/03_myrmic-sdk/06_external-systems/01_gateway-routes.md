# Gateway routes

> **Availability:** Linux runtime only

A gateway route makes a Myrmic swarm reachable to HTTP clients under a URL path. A client sends commands and events into the swarm and reads the replies, over plain HTTP or a WebSocket.

## When to use

Use a gateway when an HTTP client outside the swarm has to reach a cell. Use commands or events when one cell has to reach another.

## Operations

- Describe a route under a URL path the cell will own.
- Add the cell's message API, a WebSocket endpoint, or both.
- Register the route.
- Remove a route.

## Example

```rust
use myrmic_sdk::Metadata;

#[myrmic_sdk::init]
fn init(_md: Metadata) -> myrmic_sdk::Result {
    // "control", "/control" and "/control/" all mean the same path.
    myrmic_sdk::gateway::mount("/control")
        // At /control/api.
        .api("/api")
        // At /control/ws.
        .ws("/ws")
        // Nothing is reachable until this succeeds.
        .bind()?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn get_status(md: Metadata) -> myrmic_sdk::Result {
    // When the command arrived through the gateway, md.sender is the client's
    // session, so a command sent back to it reaches that client.
    myrmic_sdk::send(md.sender, "status", &true)?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn go_offline(_md: Metadata) -> myrmic_sdk::Result {
    // The same path the route was registered under.
    myrmic_sdk::gateway::unmount("/control")?;

    Ok(())
}
```

## Behavior

### Normal

Registering and removing a route join the handler's transaction: they take effect when the handler returns successfully, and are rolled back if it fails.

A path belongs to the cell that claimed it first. That cell can register it again, which replaces the old one. Another cell trying fails.

On the **API** path:

- a GET opens a stream, whose first message hands back a session id
- a POST sends one command or event, with that session id in a header
- replies arrive on the stream, not in the POST's response

On the **WebSocket** path, one connection carries both directions, and needs no session id of its own.

A command goes to the cell the client names, or to the cell that owns the route when none was named. An event goes to every cell that handles it.

A route stops working when the cell that owns it is gone, whether it was undeployed or lost with its node.

### Errors

Registering fails when:

- the path is invalid
- nothing was added to serve
- another cell owns the path
- the runtime cannot register it

Removing fails when:

- the path is invalid
- the cell has no such route registered
- the route is owned but another cell

### Limits

A cell that uses the gateway does not start on an embedded node. It fails to load, before any handler runs.

## API documentation

See the API documentation for [`myrmic_sdk::gateway`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/gateway/index.html), which covers every builder method and error.
