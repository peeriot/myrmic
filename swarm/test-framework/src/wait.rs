use std::time::Duration;

/// Default timeout for state-observation polling (runtime listed, SRI deployed, …).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default interval between condition polls.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What [`wait_for`] fails with: the state it waited for didn't show up in time.
#[derive(Debug, thiserror::Error)]
#[error("timed out after {timeout:?} waiting for {what}")]
pub struct Timeout {
    /// what was waited for, e.g. "SRI `x` to show up in `cells status`"
    pub what: String,
    /// how long it waited
    pub timeout: Duration,
}

/// Poll `check` every [`DEFAULT_POLL_INTERVAL`] until it yields a value, and return that value.
/// Fails with [`Timeout`], naming `what` was waited for, once `timeout` is up.
///
/// For a state that has to settle before a test can read it, e.g. a node showing up in a list:
/// the check returns `Some` with what the test needs from that state, `None` while it is not
/// there yet. A check that can fail ends the wait with its error by yielding it as the value
/// (`Some(Err(err))`), so the caller gets `Result<Result<_, E>, Timeout>`.
pub async fn wait_for<T, F, Fut>(timeout: Duration, what: &str, mut check: F) -> Result<T, Timeout>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    tokio::time::timeout(timeout, async {
        loop {
            if let Some(found) = check().await {
                return found;
            }
            tokio::time::sleep(DEFAULT_POLL_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| Timeout {
        what: what.to_owned(),
        timeout,
    })
}

/// Poll `condition` every `poll_interval` until it returns `true` or `timeout` elapses.
/// Returns whether the condition became true.
pub async fn wait_until<F, Fut>(
    timeout: Duration,
    poll_interval: Duration,
    mut condition: F,
) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(timeout, async {
        loop {
            if condition().await {
                return;
            }
            tokio::time::sleep(poll_interval).await;
        }
    })
    .await
    .is_ok()
}

/// Run `check` again and again until `duration` has passed, for a state that must hold
/// throughout, not just become true once: `check` asserts on it every time, and paces itself, by
/// waiting for the next event it checks or by sleeping between polls. Ends with the first error
/// `check` returns.
pub async fn check_throughout<E>(
    duration: Duration,
    mut check: impl AsyncFnMut() -> Result<(), E>,
) -> Result<(), E> {
    let end = tokio::time::Instant::now() + duration;
    while tokio::time::Instant::now() < end {
        check().await?;
    }
    Ok(())
}

/// Assert that an async condition becomes true within a timeout.
///
/// ```ignore
/// assert_eventually!(Duration::from_secs(10), myrmic.is_sri_deployed(&sri).await);
/// ```
#[macro_export]
macro_rules! assert_eventually {
    ($timeout:expr, $cond:expr $(, $($msg:tt)+)?) => {
        assert!(
            $crate::wait_until($timeout, $crate::wait::DEFAULT_POLL_INTERVAL, || async { $cond }).await
            $(, $($msg)+)?
        )
    };
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU32, Ordering},
        time::Duration,
    };

    use super::{check_throughout, wait_for, wait_until};

    #[tokio::test]
    async fn wait_for_returns_the_value_once_there_is_one() -> anyhow::Result<()> {
        let calls = AtomicU32::new(0);
        let found = wait_for(Duration::from_secs(5), "the third call", || async {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            (call >= 2).then_some(call)
        })
        .await?;
        assert_eq!(found, 2);
        Ok(())
    }

    #[tokio::test]
    async fn check_throughout_ends_with_the_first_error() {
        let mut calls = 0;
        let checked = check_throughout(Duration::from_secs(5), async || {
            calls += 1;
            if calls < 3 { Ok(()) } else { Err(calls) }
        })
        .await;
        assert_eq!(checked, Err(3));
    }

    #[tokio::test]
    async fn returns_true_once_condition_holds() {
        let calls = AtomicU32::new(0);
        let ok = wait_until(
            Duration::from_secs(5),
            Duration::from_millis(10),
            || async { calls.fetch_add(1, Ordering::SeqCst) >= 2 },
        )
        .await;
        assert!(ok);
        assert!(calls.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn returns_false_on_timeout() {
        let ok = wait_until(
            Duration::from_millis(50),
            Duration::from_millis(10),
            || async { false },
        )
        .await;
        assert!(!ok);
    }

    #[tokio::test]
    async fn assert_eventually_passes() {
        crate::assert_eventually!(Duration::from_secs(1), true);
    }
}
