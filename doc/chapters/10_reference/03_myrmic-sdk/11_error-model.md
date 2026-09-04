# Errors

A handler fails only when its code returns an error, either by propagating the error from a call that engages the runtime or by returning an error explicitly.

A failed call to the runtime returns `ApiError`, an enum, so the code can match a case and decide what to do.

A handler returns `myrmic_sdk::Result`, whose error is a `&'static str`. Across the WebAssembly boundary a handler can only return a number, so the SDK stores the string separately.

The runtime logs a handler's failure at error level with the returned string as the message.


## Example

```rust
use myrmic_sdk::{ApiError, Metadata, String};

#[myrmic_sdk::cmd]
fn stamp(_md: Metadata, label: String) -> myrmic_sdk::Result {
    if label.is_empty() {
        // Returned on the cell's own logic.
        return Err("a label is required");
    }

    // `ApiError` converts into the handler's error type, so `?` propagates it.
    let taken_at = myrmic_sdk::now()?;

    // The error type is `&'static str`, so it takes a literal and nothing else.
    let uptime = myrmic_sdk::uptime().map_err(|_| "the node's uptime is unavailable")?;

    myrmic_sdk::info!("{label} at {taken_at:?}, node up for {uptime:?}")?;

    Ok(())
}

fn describe(error: ApiError) -> &'static str {
    match error {
        // Worth retrying: the runtime cannot serve the call yet.
        ApiError::NotReady => "not ready",

        // The rest will fail again.
        ApiError::Usage => "incorrect use",
        ApiError::Serde(context) => context,
        ApiError::TimedOut => "timed out",
        ApiError::BufferTooSmall => "buffer too small",
        ApiError::SemQuery => "invalid query",

        // Every code the SDK does not name arrives here.
        ApiError::UnknownErrorCode(code) => {
            let _ = code;
            "unknown"
        }
    }
}
```

## Behavior

### Normal

The failure is logged at error level, with the returned string as the message.

The runtime rolls back part of what the handler did.

Rolled back:

- everything written to storage
- every command and event the handler sent
- the message the handler was given

Left in place:

- every cell the handler spawned, and every cell it terminated
- every timer it armed or cancelled
- everything it logged
- everything it wrote to an outlet, a pin, or a Bluetooth peripheral

Afterwards, what happens to the invocation depends on what it was:

- **a command** - delivered again, so a command handler has to be idempotent
- **an event** - not delivered again, so the event is lost
- **a scheduled invocation** - lost, and the schedule keeps running
- **a monitor notification** - delivered again
- **initialization** - the deployment fails with that error message, and the cell does not start

## API documentation

For every error variant, see [`ApiError`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/enum.ApiError.html). For the handler's result type, see [`Result`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/type.Result.html).
