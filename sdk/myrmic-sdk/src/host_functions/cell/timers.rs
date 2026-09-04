use core::ffi::c_int;
use core::time::Duration;

use myrmic_common::cells::{Command, CreateTimerRequest};

use crate::error::{ApiError, ErrorCode};
use crate::{ApiResult, Callback, Void};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "cell")]
    unsafe extern "C" {

        /// Creates a timer (periodic interval or one-shot delay). The payload is a
        /// serialized `CreateTimerRequest` specifying the export name and schedule.
        ///
        /// # Arguments:
        /// - `buffer`: pointer to the serialized `CreateTimerRequest`
        /// - `length`: length of the serialized request
        ///
        /// # Returns:
        /// - timer ID (>= 0) on success
        /// - negative error code on failure
        pub(super) fn create_timer(buffer: *const u8, length: c_int) -> c_int;

        /// Cancels an active timer.
        ///
        /// # Arguments:
        /// - `id`: the timer ID returned by `create_timer`
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - negative error code on failure
        pub(super) fn cancel_timer(id: c_int) -> c_int;
    }
}

/// Handle to an active interval or delay. Can be cancelled by calling `.cancel()`.
///
/// **Important:** Dropping the handle does NOT cancel the timer — the timer
/// continues running on the host. To retain the ability to cancel, store the
/// handle in cell state so it persists across handler invocations.
#[must_use = "dropping the handle loses the ability to cancel the timer — store it in cell state"]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct TimerHandle {
    id: u32,
}

impl TimerHandle {
    /// Cancels the timer, consuming the handle.
    pub fn cancel(self) -> ApiResult<()> {
        // SAFETY: Wasm linear memory is isolated — the host is responsible for
        // correct handling of the timer ID.
        unsafe { c_functions::cancel_timer(self.id as c_int) }.to_result()
    }
}

/// Creates a periodic interval that calls the named export on each tick.
#[must_use]
pub fn interval(cb: Callback<Void>, period: Duration) -> IntervalBuilder {
    IntervalBuilder {
        export_name: cb.into(),
        delay: None,
        period,
        count: None,
        fixed_delay: false,
    }
}

/// Creates a periodic interval with an initial delay before the first tick.
#[must_use]
pub fn interval_at(cb: Callback<Void>, delay: Duration, period: Duration) -> IntervalBuilder {
    IntervalBuilder {
        export_name: cb.into(),
        delay: Some(delay),
        period,
        count: None,
        fixed_delay: false,
    }
}

/// Creates a one-shot delayed action that calls the named export after the delay.
#[must_use]
pub fn delay(cb: Callback<Void>, delay: Duration) -> DelayBuilder {
    DelayBuilder {
        export_name: cb.into(),
        delay,
    }
}

/// Builder for periodic intervals. Supports optional `.count()` for finite repetition.
pub struct IntervalBuilder {
    export_name: Command,
    delay: Option<Duration>,
    period: Duration,
    count: Option<u32>,
    fixed_delay: bool,
}

impl IntervalBuilder {
    /// Limits the interval to a finite number of ticks.
    #[must_use]
    pub fn count(mut self, count: u32) -> Self {
        self.count = Some(count);
        self
    }

    /// Schedules the next interval tick after the previous exported handler returns.
    #[must_use]
    pub fn fixed_delay(mut self) -> Self {
        self.fixed_delay = true;
        self
    }

    /// Creates the interval and returns a handle.
    pub fn build(self) -> ApiResult<TimerHandle> {
        if self.count == Some(0) {
            let _ = crate::warn!("timer count of 0 is invalid — timer would never fire");
            return Err(ApiError::Usage);
        }
        let request = CreateTimerRequest {
            export_name: self.export_name.as_ref().into(),
            delay_ms: self.delay.map_or(0, |d| d.as_millis() as u64),
            period_ms: self.period.as_millis() as u64,
            count: self.count,
            fixed_delay: self.fixed_delay,
        };
        send_create_request(request)
    }
}

/// Builder for one-shot delayed actions.
pub struct DelayBuilder {
    export_name: Command,
    delay: Duration,
}

impl DelayBuilder {
    /// Creates the delay and returns a handle.
    pub fn build(self) -> ApiResult<TimerHandle> {
        let request = CreateTimerRequest {
            export_name: self.export_name.as_ref().into(),
            delay_ms: self.delay.as_millis() as u64,
            period_ms: 0,
            count: Some(1),
            fixed_delay: false,
        };
        send_create_request(request)
    }
}

fn send_create_request(request: CreateTimerRequest) -> ApiResult<TimerHandle> {
    let bytes = postcard::to_allocvec(&request).expect("serialization should not fail");
    // SAFETY: Wasm linear memory is isolated — the host reads from the
    // provided pointer/length and is responsible for correct behaviour.
    let id = unsafe { c_functions::create_timer(bytes.as_ptr(), bytes.len() as c_int) };
    if id >= 0 {
        Ok(TimerHandle { id: id as u32 })
    } else {
        Err(id.into())
    }
}
