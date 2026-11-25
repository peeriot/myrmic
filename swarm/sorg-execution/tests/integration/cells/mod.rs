mod events;
mod handler_crash;
mod init;
mod macros;
mod periodic;
mod runtime;
mod sorg_client;
mod spawning;
mod telemetry;
mod timers;

use std::time::Duration;

use sorg_tests::EventQueue;

/// Receives `periods + 1` ticks whose payload is the cell's `uptime()` in
/// milliseconds and returns the mean gap between them. The stamps are taken in
/// the cell, so event-delivery jitter between cell and test does not reach the
/// result, and averaging divides the dispatch jitter of the end ticks.
///
/// The per-receive timeout of 10s is above the 5s event poll backstop of the client.
///
/// # Errors
///
/// Returns a description when `periods` is zero, a tick does not arrive in time, or a
/// payload is not an uptime stamp.
async fn mean_tick_gap_ms(queue: &mut EventQueue, periods: u64) -> Result<u64, String> {
    if periods == 0 {
        return Err("the mean gap needs at least one period".to_owned());
    }

    let mut min = u64::MAX;
    let mut max = u64::MIN;
    for i in 0..=periods {
        let payload = tokio::time::timeout(Duration::from_secs(10), queue.receive())
            .await
            .map_err(|_| format!("timed out waiting for tick {i}"))?
            .map_err(|e| format!("error receiving tick {i}: {e}"))?;
        let stamp: u64 = serde_json::from_slice(&payload)
            .map_err(|e| format!("tick {i} payload is not an uptime stamp in ms: {e}"))?;
        min = min.min(stamp);
        max = max.max(stamp);
    }

    Ok((max - min) / periods)
}
