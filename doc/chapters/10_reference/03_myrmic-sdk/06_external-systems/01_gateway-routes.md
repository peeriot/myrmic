# Gateway routes

> **Availability:** Linux runtime only

A gateway route makes a cell reachable to HTTP clients under a URL path. Every request under that path is delivered to one of the cell's commands, which answers it: with a response, a file, or by upgrading it to a WebSocket.

## When to use

Use a gateway when an HTTP client outside the swarm has to reach a cell. Use commands or events when one cell has to reach another.

## Operations

- Register a route under a URL path the cell will own, naming the command its requests go to.
- Answer a request with a response, a file, or a WebSocket upgrade.
- Send frames to, or close, an upgraded WebSocket.
- Remove a route.

## Example

```rust
use myrmic_sdk::gateway::{self, HttpRequest, HttpResponse, StatusCode, WsEvent, WsMessage};
use myrmic_sdk::{Callback, Metadata};

#[myrmic_sdk::init]
fn init(_md: Metadata) -> myrmic_sdk::Result {
    // "control", "/control" and "/control/" all mean the same path.
    gateway::mount("/control", Callback::of::<on_http>())?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_http(_md: Metadata, req: HttpRequest) -> myrmic_sdk::Result {
    // The path is relative to the mount: /control/status arrives as /status.
    match req.path.as_str() {
        "/status" => req.respond(
            HttpResponse::new(StatusCode::OK)
                .header("content-type", "application/json")
                .body(r#"{"online":true}"#),
        ),
        // Frames on /control/ws go to on_ws.
        "/ws" => req.upgrade(Callback::of::<on_ws>()),
        _ => req.respond(HttpResponse::new(StatusCode::NOT_FOUND)),
    }
}

#[myrmic_sdk::cmd]
fn on_ws(_md: Metadata, event: WsEvent) -> myrmic_sdk::Result {
    match event {
        WsEvent::Open(socket) => socket.send(WsMessage::Text("hello".into())),
        // Echo every frame, text or binary, back to the client.
        WsEvent::Message(socket, message) => socket.send(message),
        WsEvent::Closed(_) => Ok(()),
    }
}

#[myrmic_sdk::cmd]
fn go_offline(_md: Metadata) -> myrmic_sdk::Result {
    // The same path the route was registered under.
    gateway::unmount("/control")?;

    Ok(())
}
```

## Behavior

### Normal

Registering and removing a route join the handler's transaction: they take effect when the handler returns successfully, and are rolled back if it fails.

A path belongs to the cell that claimed it first. That cell can register it again, which replaces the old one. Another cell trying fails.

A request carries its method, its path relative to the mount, the query string, its headers and its body. It is answered once; later answers are ignored. A request is an ordinary message, so a cell can send it on to another cell, which answers it instead.

Upgrading a request opens a WebSocket whose events go to the command named in the upgrade, on the cell that upgraded it. `Open` arrives first and `Closed` last, whether the client, the cell or the gateway closed it. Frames are text or binary, and arrive in order.

A `Socket` can be kept, in memory or the database, and sent to at any time while it is open. Sends to a closed socket are dropped.

A route stops working when the cell that owns it is gone, whether it was undeployed or lost with its node.

### Errors

Registering fails when:

- the path is invalid
- another cell owns the path
- the runtime cannot register it

Removing fails when:

- the path is invalid
- the cell has no such route registered
- the route is owned by another cell

The client gets:

- `504 Gateway Timeout` when no answer arrives within 30 seconds
- `426 Upgrade Required` when a request that asked for no upgrade is upgraded
- `502 Bad Gateway` when the answer is malformed

### Limits

Request bodies are limited to 1 MiB.

A socket whose client reads slower than the cell sends is closed.

A cell that uses the gateway does not start on an embedded node. It fails to load, before any handler runs.

## API documentation

See the API documentation for [`myrmic_sdk::gateway`](https://docs.myrmic.dev/myrmic_sdk/git/myrmic_sdk/gateway/index.html), which covers every type and error.
