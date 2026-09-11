# Runtime environment

> **Availability:** Linux and embedded runtimes

A cell can read facts about the runtime hosting it: the runtime's effective tag set and its id. These describe where the cell landed, not the cell itself.

## When to use

Read the runtime tags when a cell must adapt to where it runs, such as choosing a GPIO pin by board or a timer period by whether its node is battery powered.

Read the runtime id when a cell needs a stable name for its current host, for instance to report it or to pin later work to the same node.

## Operations

- Read the effective tag set of the runtime hosting this cell.
- Read the id of the runtime hosting this cell.

## Example

```rust
use myrmic_sdk::Metadata;

#[myrmic_sdk::cmd]
fn describe_host(_md: Metadata) -> myrmic_sdk::Result {
    // The same tags self-organization uses to place cells.
    let tags = myrmic_sdk::runtime_tags()?;
    if tags.iter().any(|t| t == "embedded") {
        myrmic_sdk::info!("running on an embedded node")?;
    }

    // Stable for the lifetime of the runtime process.
    let id = myrmic_sdk::runtime_id()?;
    myrmic_sdk::info!("hosted by runtime {id}")?;

    Ok(())
}
```

## Behavior

### Normal

The tags are the same set self-organization uses to place cells: the runtime's configured tags, any operator retags, and the intrinsic hardware and platform tags, such as `embedded` or the board and soc name.

The runtime id is a Zenoh id. It is stable for the lifetime of the runtime process, and is the value a deploy pins to through the `@<id>` system tag.

### Limits

The tag set is read at the moment of the call. A cell placed on another node, or a node retagged after the call, sees a different set, so nothing read here can be assumed to hold later.

The runtime id names the current host, not the cell. It changes if the cell is later placed on another runtime, and is not preserved across a runtime restart.

## API documentation

For exact signatures and every error, see [`runtime_tags`](https://docs.myrmic.dev/myrmic_sdk/git/myrmic_sdk/fn.runtime_tags.html) and [`runtime_id`](https://docs.myrmic.dev/myrmic_sdk/git/myrmic_sdk/fn.runtime_id.html).
