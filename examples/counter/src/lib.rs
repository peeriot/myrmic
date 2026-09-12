#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Callback, JsonValue, Metadata};

const STATE: State<i32> = State::new_const("my-key");

#[myrmic_sdk::init]
fn init(md: Metadata) -> myrmic_sdk::Result {
    let _ = myrmic_sdk::info!("starting (id={:?})", md.id).ok();
    Ok(())
}

#[myrmic_sdk::cmd]
fn count(md: Metadata, callback: Option<Callback<JsonValue>>) -> myrmic_sdk::Result {
    let value = STATE.load()?.unwrap_or_default();

    // `myrmic send` carries no callback, and a nil sender that could not be
    // answered even if it did.
    if let Some(callback) = callback
        && !md.sender.is_nil()
    {
        let _ = myrmic_sdk::info!("Returning count {} to (sender={:?})", value, md.sender).ok();
        callback.invoke(md.sender, &JsonValue::from(value))?;
    } else {
        let _ = myrmic_sdk::info!("Count is {} (no caller to answer)", value).ok();
    }

    Ok(())
}

#[myrmic_sdk::cmd]
fn increment(md: Metadata) -> myrmic_sdk::Result {
    let count = STATE.upsert_with(|count| {
        *count = *count + 1;
    })?;

    let _ = myrmic_sdk::info!("Incremented count to {} (sender={:?})", count, md.sender).ok();

    Ok(())
}

#[myrmic_sdk::cmd]
fn decrement(md: Metadata) -> myrmic_sdk::Result {
    let count = STATE.upsert_with(|count| {
        *count = *count - 1;
    })?;

    let _ = myrmic_sdk::info!("Decremented count to {} (sender={:?})", count, md.sender).ok();

    Ok(())
}
