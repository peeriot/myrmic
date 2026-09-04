//! Blinky demo using fixed-delay periodic scheduling.
//!
//! Toggles GPIO 0 on every tick and publishes a `blink` event. `fixed_delay`
//! ensures the next tick only starts after the GPIO toggle and event publish
//! have completed.
#![no_std]

use core::time::Duration;

use embedded_hal::digital::OutputPin;
use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::gpio::Gpio0;
use myrmic_sdk::{Callback, EventPublishRequest, Metadata, Result, publish_event};

const KV: Kv<bool> = Kv::new("blinky/");
const KEY: &str = "led_on";

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    if KV.get(KEY)?.is_none() {
        KV.put(KEY, &false)?;
    }
    let _ = myrmic_sdk::interval(Callback::of::<blink>(), Duration::from_millis(500))
        .fixed_delay()
        .build()
        .map_err(|_| "timer failed")?;
    Ok(())
}

/// Timer target: toggles GPIO 0 and publishes the new state on `blink`.
#[myrmic_sdk::cmd]
fn blink(_md: Metadata) -> Result<()> {
    let led_on = !KV.get(KEY)?.unwrap_or(false);
    KV.put(KEY, &led_on)?;

    let Some(mut gpio) = Gpio0::try_get() else {
        myrmic_sdk::error!("GPIO 0 unavailable").ok();
        return Ok(());
    };
    let result = if led_on {
        gpio.set_high()
    } else {
        gpio.set_low()
    };
    if result.is_err() {
        myrmic_sdk::error!("GPIO error").ok();
        return Ok(());
    }

    publish_event(&EventPublishRequest {
        event: "blink".try_into()?,
        payload: Some(if led_on {
            b"on".to_vec()
        } else {
            b"off".to_vec()
        }),
    })?;
    Ok(())
}
