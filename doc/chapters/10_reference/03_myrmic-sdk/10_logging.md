# Logging

> **Availability:** Linux and embedded runtimes

A cell logs what happened while it ran. Each line goes to the node's logger.

## Operations

- Log a message at one of five levels: trace, debug, info, warn, error.

## Example

```rust
use myrmic_sdk::{LogLevel, Metadata};

#[myrmic_sdk::cmd]
fn log_everything(md: Metadata) -> myrmic_sdk::Result {
    myrmic_sdk::trace!("a trace message")?;
    myrmic_sdk::debug!("a debug message")?;
    myrmic_sdk::info!("an info message")?;
    myrmic_sdk::warn!("a warning message")?;
    myrmic_sdk::error!("an error message")?;

    // The same as info!, with the level chosen in code.
    myrmic_sdk::log(&myrmic_sdk::format!("logged by {}", md.id), LogLevel::Info)?;

    Ok(())
}
```

## Behavior

### Normal

A log is not part of the handler's transaction. A handler that fails and is rolled back still leaves its lines in the log.

The runtime's log config decides what happens to a log line: which levels get through, how it is printed, and whether it is exported or stored.

### Errors

Logging fails when the runtime cannot be reached, and when the message is not a valid UTF-8 string.

### Limits

A log message is readable outside the cell, so keep secrets and sensitive data out of it.

## API documentation

For the level macros, see [`trace!`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/macro.trace.html), [`debug!`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/macro.debug.html), [`info!`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/macro.info.html), [`warn!`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/macro.warn.html) and [`error!`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/macro.error.html). For logging without a macro, see [`log`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/fn.log.html), [`LogLevel`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/enum.LogLevel.html) and [`format!`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/macro.format.html).
