//! Internal producer. Produces events for N object twins

#![no_std]

use core::time::Duration;

use myrmic_sdk::db::tree::Kv;
use myrmic_sdk::{Callback, Message, Metadata, Result, Sri, String, TimerHandle, format};

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct CameraState {
    object_cells: u16,
    zone_cells: u16,
    next_object: u16,
    next_zone: u16,
    bench_id: u64,
    call_id: u64,
    payload_size: usize,
}

#[derive(serde::Serialize, serde::Deserialize, Message)]
#[codec(myrmic_sdk::Postcard)]
struct ObjectUpdate {
    bench_id: u64,
    call_id: u64,
    zone_id: u16,
    payload: String,
}

#[derive(serde::Serialize, serde::Deserialize, Message)]
#[codec(myrmic_sdk::Postcard)]
pub struct StartRequest {
    bench_id: u64,
    call_id: u64,
    object_cells: u16,
    zone_cells: u16,
    produce_every_ms: u16,
    payload_size: usize,
    /// if `true`, the next tick's countdown only starts once the current tick's handler has
    /// actually finished — self-throttling to however long a `send` actually takes, instead of
    /// firing on a strict `produce_every_ms` schedule and letting ticks back up in the timer
    /// queue when a `send` takes longer than that.
    fixed_delay: bool,
}

const STATE: Kv<CameraState> = Kv::new("camera_state");
const STATE_KEY: &str = "current";
const TIMER: Kv<TimerHandle> = Kv::new("camera_timer");
const TIMER_KEY: &str = "active";

#[myrmic_sdk::init]
fn init(_md: Metadata) -> Result<()> {
    Ok(())
}

#[myrmic_sdk::cmd]
fn start_producing(_md: Metadata, req: StartRequest) -> Result<()> {
    if TIMER.get(TIMER_KEY)?.is_some() {
        // already producing; stop before starting again
        return Ok(());
    }

    let StartRequest {
        bench_id,
        call_id,
        object_cells,
        zone_cells,
        produce_every_ms,
        payload_size,
        fixed_delay,
    } = req;

    if object_cells == 0 {
        return Err("object_cells must be > 0");
    }
    if zone_cells == 0 {
        return Err("zone_cells must be > 0");
    }
    if produce_every_ms == 0 {
        return Err("produce_every_ms must be > 0");
    }

    STATE.put(
        STATE_KEY,
        &CameraState {
            object_cells,
            zone_cells,
            next_object: 0,
            next_zone: 0,
            bench_id,
            call_id,
            payload_size,
        },
    )?;

    let mut builder = myrmic_sdk::interval(
        Callback::of::<tick>(),
        Duration::from_millis(u64::from(produce_every_ms)),
    );
    if fixed_delay {
        builder = builder.fixed_delay();
    }
    let handle = builder.build()?;
    TIMER.put(TIMER_KEY, &handle)?;

    Ok(())
}

#[myrmic_sdk::cmd]
fn stop_producing(_md: Metadata) -> Result<()> {
    if let Some(handle) = TIMER.get(TIMER_KEY)? {
        TIMER.delete(TIMER_KEY)?;
        let _ = handle.cancel();
    }
    STATE.delete(STATE_KEY)?;

    Ok(())
}

/// Timer tick handler. Sends the next `update` command in the object/zone
/// round-robin.
#[myrmic_sdk::cmd]
fn tick(_md: Metadata) -> Result<()> {
    let Some(mut state) = STATE.get(STATE_KEY)? else {
        return Ok(());
    };

    // the object and zone SRI to go through, in a future version this
    // could be a random number too (based on a strategy config maybe)
    let object = state.next_object;
    state.next_object += 1;
    if state.next_object % state.object_cells == 0 {
        state.next_object = 0;
    }
    let zone = state.next_zone;
    state.next_zone += 1;
    if state.next_zone % state.zone_cells == 0 {
        state.next_zone = 0;
    }

    // increment the call id
    let call_id = state.call_id;
    state.call_id += 1;

    let bench_id = state.bench_id;
    let payload_size = state.payload_size;

    STATE.put(STATE_KEY, &state)?;

    // build object target SRI and request
    let target =
        Sri::of_path(&format!("asset.object.{object}")).map_err(|_| "invalid object sri")?;
    let req = ObjectUpdate {
        bench_id,
        call_id,
        zone_id: zone,
        payload: "x".repeat(payload_size),
    };

    myrmic_sdk::send(target, "update", &req)?;

    Ok(())
}
