# myrmic gateway

## Name

`myrmic gateway` - Start a swarm gateway node

## Synopsis

```
myrmic gateway [OPTIONS]
```

## Description

Start a minimal swarm gateway node. The gateway exposes the swarm to the external world - it delivers HTTP requests and WebSocket frames from external clients to cells, and serves static web
assets from the blob store when a cell asks it to.

A gateway can also run embedded inside a Myrmic runtime - see [runtime configuration](../01_configuration/01_runtime-configuration.md#gateway).

The gateway serves no application of its own. Cells declare how they should be served, and every gateway in the network picks the routes up from the shared route registry and serves them alike.

### Cell-declared routes

A cell declares a route by calling `myrmic_sdk::gateway::mount`, usually from its `init` handler. Every request under the mount is delivered to the command it names, and the cell decides how
to answer:

```rust
use myrmic_sdk::gateway::{self, Asset, HttpRequest, WsEvent};
use myrmic_sdk::{Callback, Metadata};

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result<()> {
    gateway::assets(md.id).put("/index.html", include_bytes!("../dist/index.html"))?;
    gateway::mount("/chat", Callback::of::<on_http>())?;
    Ok(())
}

#[myrmic_sdk::cmd]
fn on_http(_md: Metadata, req: HttpRequest<'_>) -> myrmic_sdk::Result<()> {
    if req.path == "/ws" {
        // Accept the WebSocket; its frames go to `on_ws`.
        return req.upgrade(Callback::of::<on_ws>());
    }
    // Serve a file from the asset store.
    let asset = Asset::new(req.path).index("/index.html");
    req.asset(asset)
}

#[myrmic_sdk::cmd]
fn on_ws(_md: Metadata, event: WsEvent) -> myrmic_sdk::Result<()> {
    match event {
        WsEvent::Message(socket, message) => socket.send(message),
        WsEvent::Open(_) | WsEvent::Closed(_) => Ok(()),
    }
}
```

Serving assets is as easy as just uploading them to the blob-store, with a convenience function via (`myrmic_sdk::gateway::assets`), and answering with an `Asset`. The cell can populate the store
however it likes. (compiled in with `include_bytes!`, fetched from another cell, or written at runtime.)

A request that gets no answer within 30 seconds is answered with `504 Gateway Timeout`.

The lifetime of the routing configuration is tied to the cell that declared it. If the owning cell goes, the routing config does as well.

By default, mounts are first-come. A cell may re-mount its own path freely, but mounting a path another cell holds will fail.

A common pitfall is when embedding assets into the cell, you might need to increase the memory limits acoordingly
(see [Cell and Application Configuration](../01_configuration/02_cell-and-application-configuration.md)).

### Routing configuration

As mentioned, the default configuration for a gateway acts on a first-come basis. The gateway can, however, declare a routing file, which is monitored, and acts as an authority. It can declare which
cell owns which routes, and OIDC configuration.

```json
{
  "routes": {
	"/chat": {
	  "srn": "chatty",
	  "oidc": {
		"application_base_url": "https://chat.example.org",
		"issuer": "https://id.example.org/realms/myrmic",
		"client_id": "chatty",
		"client_secret": "…",
		"scopes": [
		  "openid",
		  "profile",
		  "email"
		]
	  }
	}
  }
}
```

When a `routes` block is declared, it acts as an allow-list. Only serving mounts that appear in this block. This permits the gateway administator to control what resources should be accessible.

The `srn` pins it to a specific cell, but can be omitted. The `oidc` block enables OIDC support for that route, and will only permit users that have the correct permissions.

## Options

`-p` / `--port PORT`

TCP port to bind the gateway on. Defaults to `8080`.

`--routing PATH`

Path to the routing configuration as described above. Without it, there's no restrictions in place.

`--over-https`

Serve behind HTTPS: session cookies are marked `Secure`, so browsers only send them back over TLS. Set this whenever a TLS terminator sits in front of the gateway.

`--session-inactivity DURATION`

How long a session may sit idle before it expires, written as a human duration (`30s`, `2m`, `1h`). Defaults to two minutes, and is never shorter than two seconds.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples

1. Start the gateway on the default port:

```bash
myrmic gateway
```

2. Start the gateway on a custom port:

```bash
myrmic gateway --port 9090
```

3. Serve only the routes in a configuration file, behind a TLS terminator, with a longer idle window:

```bash
myrmic gateway --routing ./routing.json --over-https --session-inactivity 15m
```
