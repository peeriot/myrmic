use alloc::string::String;

use bitflags::bitflags;
use embassy_futures::select::{Either3, select3};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, signal::Signal,
};
use embassy_time::{Duration, Instant, Timer};
use myrmic_common::cells::CreateTimerRequest;

use crate::async_request::Context;
use crate::async_request::cell_host::CELL_MSG_CHANNEL;
use crate::cell::CellMessage;

pub(crate) const MAX_TIMERS: usize = 5;

bitflags! {
    #[derive(Clone, Copy, Default)]
    pub(crate) struct TimerFlags: u8 {
        const FIXED_DELAY          = 0b01;
        const AWAITING_COMPLETION  = 0b10;
    }
}

/// Channel for timer commands
// Note: Capacity of 2 is enough, since WAMR requests are synchronous,
// so only one is ever in-flight, 2 gives a small buffer
static TIMER_COMMANDS: Channel<CriticalSectionRawMutex, TimerCommand, 2> = Channel::new();

/// Result of a `Create` command, signalled back to the awaiting `create()` caller. WAMR is
/// synchronous and only one host request is in flight at a time, so a single shared `Signal`
/// is safe — every signal has a matching awaiter.
static CREATE_RESPONSE: Signal<CriticalSectionRawMutex, Result<(), &'static str>> = Signal::new();

/// Result of a `Cancel` command — same single-in-flight guarantee applies.
static CANCEL_RESPONSE: Signal<CriticalSectionRawMutex, Result<(), &'static str>> = Signal::new();

static TIMER_COMPLETION: Channel<CriticalSectionRawMutex, TimerId, MAX_TIMERS> = Channel::new();

pub(crate) type TimerCompletion = u32;

/// Signals the timer manager that a fixed-delay handler has finished.
/// Sends to `TIMER_COMPLETION` when dropped, so the next tick is scheduled.
#[derive(Debug)]
pub struct TimerCompletionGuard(pub u32);

impl Drop for TimerCompletionGuard {
    fn drop(&mut self) {
        if TIMER_COMPLETION.try_send(TimerId(self.0)).is_err() {
            log::error!("timer completion channel full — fixed-delay timer may stall");
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TimerId(pub(crate) u32);

impl TimerId {
    pub(crate) fn update_to_next(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

struct TimerEntry {
    id: TimerId,
    export_name: heapless::String<64>,
    next_fire: Instant,
    /// None = one-shot
    period: Option<Duration>,
    /// None = infinite
    remaining: Option<u32>,
    flags: TimerFlags,
}

impl TimerFlags {
    fn for_timer(fixed_delay: bool) -> Self {
        if fixed_delay {
            Self::FIXED_DELAY
        } else {
            Self::empty()
        }
    }
}

impl TimerEntry {
    fn is_fixed_delay(&self) -> bool {
        self.flags.contains(TimerFlags::FIXED_DELAY)
    }

    fn is_awaiting_completion(&self) -> bool {
        self.flags.contains(TimerFlags::AWAITING_COMPLETION)
    }

    fn set_awaiting_completion(&mut self, value: bool) {
        self.flags.set(TimerFlags::AWAITING_COMPLETION, value);
    }
}

pub(crate) enum TimerCommand {
    Create {
        id: TimerId,
        export_name: heapless::String<64>,
        initial_delay: Duration,
        /// None = one-shot
        period: Option<Duration>,
        /// None = infinite
        count: Option<u32>,
        flags: TimerFlags,
    },
    Cancel {
        id: TimerId,
    },
}

#[derive(Debug)]
pub(crate) struct TimersContext {
    pub next_id: TimerId,
}

pub(crate) async fn create(ctx: &mut Context, req: CreateTimerRequest) -> Result<TimerId, String> {
    if req.count == Some(0) {
        return Err(alloc::string::String::from("count=0 is invalid"));
    }

    let id = ctx.timers.next_id;

    let export_name = heapless::String::try_from(req.export_name.as_str())
        .map_err(|_err| alloc::string::String::from("export name too long"))?;

    let initial_delay = Duration::from_millis(req.delay_ms);
    let period = if req.period_ms > 0 {
        Some(Duration::from_millis(req.period_ms))
    } else {
        None
    };

    // Clear any stale signal from a prior request before submitting our own. Then wait for
    // the manager task to confirm whether the timer slot was actually accepted, so the wasm
    // caller doesn't receive a handle to a timer that was silently dropped.
    CREATE_RESPONSE.reset();
    TIMER_COMMANDS
        .send(TimerCommand::Create {
            id,
            export_name,
            initial_delay,
            period,
            count: req.count,
            flags: TimerFlags::for_timer(req.fixed_delay),
        })
        .await;

    match CREATE_RESPONSE.wait().await {
        Ok(()) => {
            ctx.timers.next_id.update_to_next();
            Ok(id)
        }
        Err(msg) => Err(String::from(msg)),
    }
}

pub(crate) async fn cancel(id: TimerId) -> Result<(), String> {
    CANCEL_RESPONSE.reset();
    TIMER_COMMANDS.send(TimerCommand::Cancel { id }).await;
    match CANCEL_RESPONSE.wait().await {
        Ok(()) => Ok(()),
        Err(msg) => Err(String::from(msg)),
    }
}

#[embassy_executor::task]
pub async fn timer_manager_task() {
    let mut timers: heapless::Vec<TimerEntry, MAX_TIMERS> = heapless::Vec::new();

    loop {
        let next_active = timers
            .iter()
            .filter(|t| !t.is_awaiting_completion())
            .map(|t| t.next_fire)
            .min();

        let tick_fut = async {
            match next_active {
                Some(t) => Timer::at(t).await,
                None => core::future::pending().await,
            }
        };

        match select3(
            tick_fut,
            TIMER_COMMANDS.receive(),
            TIMER_COMPLETION.receive(),
        )
        .await
        {
            Either3::First(()) => fire_due_timers(&mut timers),
            Either3::Second(cmd) => handle_command(&mut timers, cmd),
            Either3::Third(id) => complete_fixed_delay(&mut timers, id),
        }
    }
}

fn complete_fixed_delay(timers: &mut heapless::Vec<TimerEntry, MAX_TIMERS>, id: TimerId) {
    if let Some(t) = timers.iter_mut().find(|t| t.id.0 == id.0) {
        t.set_awaiting_completion(false);
        if let Some(period) = t.period {
            t.next_fire = Instant::now() + period;
        }
    }
}

fn handle_command(timers: &mut heapless::Vec<TimerEntry, MAX_TIMERS>, cmd: TimerCommand) {
    match cmd {
        TimerCommand::Create {
            id,
            export_name,
            initial_delay,
            period,
            count,
            flags,
        } => {
            let entry = TimerEntry {
                id,
                export_name,
                next_fire: Instant::now() + initial_delay,
                period,
                remaining: count,
                flags,
            };
            match timers.push(entry) {
                Ok(()) => CREATE_RESPONSE.signal(Ok(())),
                Err(_) => {
                    log::error!("timer slots full");
                    CREATE_RESPONSE.signal(Err("timer slots full"));
                }
            }
        }
        TimerCommand::Cancel { id } => {
            let before = timers.len();
            timers.retain(|t| t.id.0 != id.0);
            if timers.len() < before {
                CANCEL_RESPONSE.signal(Ok(()));
            } else {
                CANCEL_RESPONSE.signal(Err("timer not found"));
            }
        }
    }
}

fn fire_due_timers(timers: &mut heapless::Vec<TimerEntry, MAX_TIMERS>) {
    let now = Instant::now();
    let mut i = 0;
    while i < timers.len() {
        if timers[i].next_fire <= now && !timers[i].is_awaiting_completion() {
            let fixed_delay = timers[i].is_fixed_delay();
            let completed = fixed_delay.then(|| timers[i].id.0);
            let tick_sent = send_tick(&timers[i].export_name, completed);

            let keep = match timers[i].period {
                None => false, // one-shot
                Some(period) => {
                    if fixed_delay {
                        if tick_sent {
                            timers[i].set_awaiting_completion(true);
                        } else {
                            timers[i].next_fire = Instant::now() + period;
                        }
                    } else {
                        // Advance past `now` in one step to avoid catch-up bursts.
                        while timers[i].next_fire <= now {
                            timers[i].next_fire += period;
                        }
                    }
                    match timers[i].remaining.as_mut() {
                        None => true, // infinite
                        Some(rem) => {
                            *rem -= 1; // no underflow: create() rejects count=0
                            *rem > 0
                        }
                    }
                }
            };

            if keep {
                i += 1;
            } else {
                timers.remove(i);
            }
        } else {
            i += 1;
        }
    }
}

fn send_tick(export_name: &heapless::String<64>, completed: Option<TimerCompletion>) -> bool {
    let msg = CellMessage::TimerTick {
        export_name: export_name.clone(),
        completed,
    };
    // Non-blocking try_send — if the cell message channel is full, drop the tick
    if CELL_MSG_CHANNEL.try_send(msg).is_err() {
        log::debug!("timer tick dropped — cell message channel full");
        false
    } else {
        true
    }
}
