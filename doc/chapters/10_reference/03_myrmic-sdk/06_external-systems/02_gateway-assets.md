# Gateway assets

> **Availability:** Linux runtime only

A gateway route can serve files from a cell's blob storage.

## When to use

Serve files when an HTTP client needs to download them, such as a user interface or data a cell produced.

Use the message API when that client needs to send commands or events instead.

## Operations

- Open the asset store belonging to a cell.
- Store each file at the path it is served from.
- Serve those files as part of a route, naming an index, a fallback, or another scope.

## Example

```rust
use myrmic_sdk::gateway::Fallback;
use myrmic_sdk::Metadata;

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result {
    let assets = myrmic_sdk::gateway::assets(md.id);

    assets.put("/index.html", include_bytes!("../dist/index.html"))?;

    myrmic_sdk::gateway::mount("/app")
        // Switches on file serving, and serves this file at /app/.
        .index("/index.html")
        // The default: a path matching no file gets the index, which is what a
        // single-page app needs. Use Fallback::None for a 404 instead.
        .fallback(Fallback::Spa)
        // In case the files live in another scope rather than this cell's own.
        // .scope(Scope::public("shared-assets"))
        .bind()?;

    Ok(())
}
```

## Behavior

### Normal

A cell's files are deleted when the cell is undeployed. If it is lost with its node instead, the files are not deleted.

Writing to a path that already holds a file replaces what the gateway serves. The route does not need registering again.

### Errors

Storing a gateway asset fails when:

- the scope is invalid
- storage cannot be reached

### Limits

Nothing checks that the index file exists. A missing one returns a not-found on the first request for it.

A cell that uses the gateway does not start on an embedded node. It fails to load, before any handler runs.

## API documentation

See the API documentation for [`myrmic_sdk::gateway`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/gateway/index.html), which covers every route option.
