//! Example cell that creates and cancels timers via commands.
//! Used by the timer integration tests.

#![no_std]

use core::time::Duration;

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{
    Callback, EventPublishRequest, JsonValue, Metadata, Result, TimerHandle, publish, publish_event,
};

// Persist the active handle so `cancel_timer` can reload it in a later command
// invocation — a dropped `TimerHandle` keeps ticking on the host.
const TIMERS: Kv<TimerHandle> = Kv::new("timers");
const HANDLE_KEY: &str = "active";

// `cancel_timer` reports its outcome on this event: `b"ok"` on a successful cancel, `b"err"` when
// there is no active handle or the host rejects the cancel (e.g. an already-expired one-shot).
// Commands are fire-and-forget, so this event is the only way the host observes the result.
const CANCEL_RESULT_EVENT: &str = "timer_cancel_result";

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    Ok(())
}

/// Creates a periodic timer (period: 200ms, immediate start, infinite).
#[myrmic_sdk::cmd]
fn start_periodic(_md: Metadata) -> Result<()> {
    let handle =
        myrmic_sdk::interval(Callback::of::<tick>(), Duration::from_millis(200)).build()?;
    TIMERS.put(HANDLE_KEY, &handle)?;
    Ok(())
}

/// Creates a delayed one-shot (delay: 500ms).
#[myrmic_sdk::cmd]
fn start_delayed(_md: Metadata) -> Result<()> {
    let handle = myrmic_sdk::delay(Callback::of::<tick>(), Duration::from_millis(500)).build()?;
    TIMERS.put(HANDLE_KEY, &handle)?;
    Ok(())
}

/// Creates a periodic timer with count=3 (period: 200ms).
#[myrmic_sdk::cmd]
fn start_counted(_md: Metadata) -> Result<()> {
    let handle = myrmic_sdk::interval(Callback::of::<tick>(), Duration::from_millis(200))
        .count(3)
        .build()?;
    TIMERS.put(HANDLE_KEY, &handle)?;
    Ok(())
}

/// Cancels the currently active timer, publishing the outcome on the
/// [`CANCEL_RESULT_EVENT`] event so the host can observe success vs. failure.
#[myrmic_sdk::cmd]
fn cancel_timer(_md: Metadata) -> Result<()> {
    let cancelled = match TIMERS.get(HANDLE_KEY)? {
        Some(handle) => {
            TIMERS.delete(HANDLE_KEY)?;
            handle.cancel().is_ok()
        }
        None => false,
    };

    let outcome: &[u8] = if cancelled { b"ok" } else { b"err" };
    publish_event(&EventPublishRequest {
        event: CANCEL_RESULT_EVENT.try_into()?,
        payload: Some(outcome.to_vec()),
    })?;

    Ok(())
}

/// Creates a periodic timer with an initial delay (delay: 300ms, period: 200ms).
#[myrmic_sdk::cmd]
fn start_delayed_periodic(_md: Metadata) -> Result<()> {
    let handle = myrmic_sdk::interval_at(
        Callback::of::<tick>(),
        Duration::from_millis(300),
        Duration::from_millis(200),
    )
    .build()?;
    TIMERS.put(HANDLE_KEY, &handle)?;
    Ok(())
}

/// Tries to create a timer with a non-existent export name.
#[myrmic_sdk::cmd]
fn start_invalid(_md: Metadata) -> Result<()> {
    let _handle = myrmic_sdk::interval(
        Callback::to("nonexistent_export")?,
        Duration::from_millis(200),
    )
    .build()?;
    Ok(())
}

/// The timer target invoked by every timer above. Timers invoke a `#[cmd]`
/// handler via `Callback`, so this is a command, not a bare export. Publishes a
/// `timer_tick` event.
#[myrmic_sdk::cmd]
fn tick(_md: Metadata) -> Result<()> {
    publish("timer_tick", &JsonValue::from("tick"))?;
    Ok(())
}
