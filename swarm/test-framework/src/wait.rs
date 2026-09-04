use std::time::Duration;

/// Default timeout for state-observation polling (runtime listed, SRI deployed, …).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default interval between condition polls.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::wait_until;

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
