# Gateway assets

> **Availability:** Linux runtime only

A cell can answer a gateway request with a file from blob storage. The gateway reads the file and sends it, so the cell never loads it.

## When to use

Serve files when an HTTP client needs to download them, such as a user interface or data a cell produced.

Answer with a response instead when the cell computes what the client gets.

## Operations

- Open the asset store belonging to a cell.
- Store each file at the path it is served from.
- Answer a request with an asset, naming an index, a fallback, or another scope.

## Example

```rust
use myrmic_sdk::gateway::{self, Asset, Fallback, HttpRequest};
use myrmic_sdk::{Callback, Metadata};

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result {
    let assets = gateway::assets(md.id);

    assets.put("/index.html", include_bytes!("../dist/index.html"))?;

    gateway::mount("/app", Callback::of::<on_http>())?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn on_http(_md: Metadata, req: HttpRequest) -> myrmic_sdk::Result {
    let asset = Asset::new(req.path.clone())
        // Served at /app/, and for any directory path.
        .index("/index.html")
        // The default: a path matching no file gets the index, which is what a
        // single-page app needs. Use Fallback::None for a 404 instead.
        .fallback(Fallback::Spa);
        // In case the files live in another scope rather than this cell's own.
        // .scope(Scope::public("shared-assets"))

    req.asset(asset)
}
```

## Behavior

### Normal

An asset with no scope is read from the asset store of the cell that answers with it. A scope is resolved against that cell, as its own database calls would be: a private scope is its own, and system scopes are refused.

A request for the bare mount, such as `/app`, is redirected to `/app/`, so relative links in the index resolve under the mount.

A cell's files are deleted when the cell is undeployed. If it is lost with its node instead, the files are not deleted.

Writing to a path that already holds a file replaces what the gateway serves.

### Errors

Storing a gateway asset fails when:

- the scope is invalid
- storage cannot be reached

The client gets:

- `404 Not Found` when no file matches and the fallback doesn't apply
- `502 Bad Gateway` when the asset names a scope the cell may not read
- `503 Service Unavailable` when storage cannot be reached

### Limits

Nothing checks that the index file exists. A missing one returns a not-found on the first request for it.

A cell that uses the gateway does not start on an embedded node. It fails to load, before any handler runs.

## API documentation

See the API documentation for [`myrmic_sdk::gateway`](https://docs.myrmic.dev/myrmic_sdk/git/myrmic_sdk/gateway/index.html), which covers every asset option.
