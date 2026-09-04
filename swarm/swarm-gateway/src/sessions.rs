use crate::HTTP_SESSION_GRACE;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use myrmic_common::cells::Sri;
use sorg_common::{Mailbox, remove_placement};

use tokio::time::Instant;
use uuid::Uuid;
use zenoh::Session;

/// Live HTTP sessions, keyed by session UUID (which is also the session SRI).
/// Shared between the request handlers, the per-stream drop guard, and the
/// reaper task. `WebSocket` sessions are not tracked here — a WS connection owns
/// its session for the socket's lifetime and tears it down synchronously.
pub type Sessions = Arc<Mutex<HashMap<Uuid, SessionState>>>;

/// Bookkeeping for one HTTP session.
pub struct SessionState {
    /// Number of SSE streams currently attached. While non-zero the session is
    /// never reaped; when it drops to zero the grace countdown begins.
    pub active_streams: u32,
    /// When to reap the session once no streams remain.
    pub deadline: Instant,
}

impl SessionState {
    /// Whether a session with no attached streams is past its grace deadline and so
    /// eligible for reaping.
    pub fn is_reapable(&self, now: Instant) -> bool {
        self.active_streams == 0 && self.deadline <= now
    }
}

/// Periodically evicts HTTP sessions whose grace window has elapsed, releasing
/// their cell registration and clearing any residual mailbox. This is the
/// deferred equivalent of the `WebSocket` teardown, which runs synchronously on
/// socket close.
pub fn spawn_session_reaper(session: &Session) -> (Sessions, tokio::task::JoinHandle<()>) {
    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));

    let handle = tokio::spawn({
        let session = session.clone();
        let sessions = sessions.clone();

        async move {
            let mut tick = tokio::time::interval(HTTP_SESSION_GRACE / 3);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Consume the immediate first tick so we do not scan before any session
            // could have expired.
            tick.tick().await;

            loop {
                tick.tick().await;
                let now = Instant::now();

                let expired: Vec<Uuid> = {
                    let Ok(mut map) = sessions.lock() else {
                        continue;
                    };
                    let ids: Vec<Uuid> = map
                        .iter()
                        .filter(|(_, state)| state.is_reapable(now))
                        .map(|(id, _)| *id)
                        .collect();
                    for id in &ids {
                        map.remove(id);
                    }
                    ids
                };

                for id in expired {
                    let sri = Sri::from_uuid(id);
                    let _ = remove_placement(&session, &sri).await;
                    let _ = Mailbox::new(&session).drain_commands(sri).await;
                    tracing::debug!("http session reaped: {sri}");
                }
            }
        }
    });

    (sessions, handle)
}

/// Records a brand-new HTTP session with one attached stream.
pub fn register(sessions: &Sessions, id: Uuid) {
    if let Ok(mut map) = sessions.lock() {
        map.insert(
            id,
            SessionState {
                active_streams: 1,
                deadline: Instant::now() + HTTP_SESSION_GRACE,
            },
        );
    }
}

/// Attaches a new stream to an existing session, refreshing its deadline.
/// Returns `false` if the session is unknown (already reaped) — the caller then
/// mints a fresh one.
pub fn attach_stream(sessions: &Sessions, id: Uuid) -> bool {
    let Ok(mut map) = sessions.lock() else {
        return false;
    };
    match map.get_mut(&id) {
        Some(state) => {
            state.active_streams += 1;
            state.deadline = Instant::now() + HTTP_SESSION_GRACE;
            true
        }
        None => false,
    }
}

/// Refreshes a session's grace deadline on activity (a `POST`). Returns `false`
/// if the session is unknown.
pub fn touch(sessions: &Sessions, id: Uuid) -> bool {
    let Ok(mut map) = sessions.lock() else {
        return false;
    };
    match map.get_mut(&id) {
        Some(state) => {
            state.deadline = Instant::now() + HTTP_SESSION_GRACE;
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reapable_predicate() {
        let now = Instant::now();
        let grace = std::time::Duration::from_secs(1);

        // No streams and the deadline has passed → reap.
        assert!(
            SessionState {
                active_streams: 0,
                deadline: now,
            }
            .is_reapable(now)
        );
        // A live stream is never reaped, even past the deadline.
        assert!(
            !SessionState {
                active_streams: 1,
                deadline: now,
            }
            .is_reapable(now)
        );
        // No streams but still inside the grace window → keep.
        assert!(
            !SessionState {
                active_streams: 0,
                deadline: now + grace,
            }
            .is_reapable(now)
        );
    }
}
