# Blob store

> **Availability:** Linux and embedded runtimes

The blob store holds large binary content, such as files, under file-like paths. A store handle is bound to one scope.

## Operations

- Declare a store on a scope.
- Store content at a path.
- Store several paths at once.
- Read the content at a path, all of it or a range.
- Read the size at a path without reading the content.
- List every path in the scope.
- Delete a path, or rename it.

## Example

```rust
use myrmic_sdk::db::store::BlobStore;
use myrmic_sdk::db::Scope;
use myrmic_sdk::Metadata;

#[myrmic_sdk::cmd]
fn install_assets(_md: Metadata) -> myrmic_sdk::Result {
    let store = BlobStore::new(Scope::private());

    // A path always starts with one slash, so "index.html" and "/index.html"
    // are the same file.
    store.put("/index.html", b"<html></html>")?;

    // Several at once, each stored like the call above.
    store.upload(&[
        ("/app.js", b"console.log(1)".as_slice()),
        ("/app.css", b"body{}".as_slice()),
    ])?;

    store.rename("/index.html", "/home.html")?;

    // The size alone, without reading the content.
    let size = store.size_of("/home.html")?.unwrap_or_default();

    // The whole file. Nothing at the path returns nothing, not an error.
    let _page = store.get("/home.html")?;

    // Or a piece of it, for a file too large to hold at once.
    let _head = store.get_range("/home.html", 0, 512)?;

    for path in store.list()? {
        myrmic_sdk::info!("{path} of {size} bytes")?;
    }

    // Deleting a path that is not there does not raise an error.
    store.delete("/app.css")?;

    Ok(())
}
```

## Behavior

### Normal

Every operation applies to the store's one scope and reaches nothing outside it, even when another scope holds the same path or the same content.

Storing content at a path replaces what was there.

Storing, renaming, and deleting take effect when the handler returns successfully, together with everything else it wrote, and are rolled back if the handler fails.

Renaming or deleting a path changes only the name. The content is not copied, moved, or removed.

### Errors

Every operation fails when the scope is invalid or storage cannot be reached. Beyond that:

- Storing fails when the path, with its scope, does not fit in 1 KiB.
- Listing fails when the paths do not fit in 8 KiB.
- Renaming fails when the path it starts from does not exist.

### Limits

Reading a whole file holds all of it in the cell's memory, so a large file can exhaust it. Read a range instead.

Nothing ever deletes stored content. Once its last path is gone it cannot be reached any more, and it stays on the node.

## API documentation

See the API documentation for [`myrmic_sdk::db::store`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/store/index.html), which covers every blob store operation.
