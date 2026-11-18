use crate::HTTP_SESSION_GRACE;

use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::sync::{Arc, Mutex};

use cell_protocol::Gen;
use myrmic_common::cells::Sri;
use openidconnect::CsrfToken;
use serde::{Serialize, Serializer};
use sorg_common::{Mailbox, remove_placement};

use tokio::time::Instant;
use uuid::Uuid;
use zenoh::Session;

/// Live HTTP sessions, keyed by their secret token; the session SRI lives in
/// the state. Shared between the request handlers, the per-stream drop guard,
/// and the reaper task. `WebSocket` sessions are not tracked here: a WS
/// connection owns its session for the socket's lifetime and tears it down
/// synchronously.
pub type Sessions = Arc<Mutex<SessionTable>>;

/// The session map plus a side index of how many live sessions hold each
/// browser binding, so both change under one lock.
#[derive(Default)]
pub struct SessionTable {
    map: HashMap<Secret, SessionState>,
    bindings: HashMap<Secret, usize>,
}

impl SessionTable {
    /// Refreshes the grace countdown when the last stream of the session behind
    /// `token` goes away.
    pub fn release_stream(&mut self, token: &Secret) {
        if let Some(state) = self.map.get_mut(token) {
            state.active_streams = state.active_streams.saturating_sub(1);
            if state.active_streams == 0 {
                state.deadline = Instant::now() + HTTP_SESSION_GRACE;
            }
        }
    }

    /// Removes the reapable sessions and returns the SRI and generation each
    /// one holds a placement under.
    pub fn take_expired(&mut self, now: Instant) -> Vec<(Sri, Gen)> {
        let expired: Vec<SessionState> = self
            .map
            .extract_if(|_, state| state.is_reapable(now))
            .map(|(_, state)| state)
            .collect();

        expired
            .into_iter()
            .map(|state| {
                self.unbind(&state.binding);

                (Sri::from_uuid(state.sri), state.gen_id)
            })
            .collect()
    }

    fn insert(&mut self, token: Secret, state: SessionState) {
        *self.bindings.entry(state.binding.clone()).or_default() += 1;
        if let Some(old) = self.map.insert(token, state) {
            self.unbind(&old.binding);
        }
    }

    /// The first presented value that is the binding of a live session (any
    /// mount, any owner: the binding is per browser), so a browser keeps one
    /// binding across streams. `presented` is already capped, so this is at
    /// most that many hashed lookups, whatever the number of sessions.
    fn live_binding(&self, presented: &[String]) -> Option<Secret> {
        presented.iter().find_map(|value| {
            self.bindings
                .get_key_value(value.as_str())
                .map(|(binding, _)| binding.clone())
        })
    }

    fn unbind(&mut self, binding: &Secret) {
        if let Some(count) = self.bindings.get_mut(binding) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.bindings.remove(binding);
            }
        }
    }

    fn accepted(
        &mut self,
        token: &str,
        presented: &[String],
        mount: &str,
        owner: Sri,
    ) -> Option<&mut SessionState> {
        self.map
            .get_mut(token)
            .filter(|state| state.accepts(presented, mount, owner))
    }

    #[cfg(test)]
    pub fn get(&self, token: &str) -> Option<&SessionState> {
        self.map.get(token)
    }
}

/// Bookkeeping for one HTTP session.
pub struct SessionState {
    /// Number of SSE streams currently attached. While non-zero the session is
    /// never reaped; when it drops to zero the grace countdown begins.
    pub active_streams: u32,
    /// When to reap the session once no streams remain.
    pub deadline: Instant,
    /// Generation of the session's placeholder placement, so the reaper
    /// releases the row it registered and nothing that reused the SRI since.
    pub gen_id: Gen,
    /// The session's SRI: what cells reply to and see as the sender.
    sri: Uuid,
    /// The mount the session was minted on.
    mount: String,
    /// The owner cell of that mount when the session was minted.
    owner: Sri,
    /// The browser binding; a request must present it as a `myrmic_gw` cookie.
    binding: Secret,
}

impl SessionState {
    /// Whether a session with no attached streams is past its grace deadline and so
    /// eligible for reaping.
    pub fn is_reapable(&self, now: Instant) -> bool {
        self.active_streams == 0 && self.deadline <= now
    }

    /// Whether a request on `mount` of `owner` presenting these cookie values
    /// (already capped at `MAX_PRESENTED_BINDINGS`) may use this session.
    fn accepts(&self, presented: &[String], mount: &str, owner: Sri) -> bool {
        presented
            .iter()
            .any(|value| ct_eq(value, self.binding.expose()))
            && self.mount == mount
            && self.owner == owner
    }
}

/// A session token or browser binding.
///
/// Has no `Display` and a redacting `Debug`, so it cannot end up in a log line
/// or error body by accident. [`Secret::expose`] is the only way to the value.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Secret(String);

impl Secret {
    /// A fresh 256-bit secret; the only source of tokens and bindings.
    pub fn random() -> Self {
        Self(CsrfToken::new_random_len(32).into_secret())
    }

    /// The value; only for the binding compare, the SSE `id:` and `Set-Cookie`.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Secret {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Exists only so `Ready.token` can carry the token on the wire.
impl Serialize for Secret {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl Debug for Secret {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// Periodically evicts HTTP sessions whose grace window has elapsed, releasing
/// their cell registration and clearing any residual mailbox. This is the
/// deferred equivalent of the `WebSocket` teardown, which runs synchronously on
/// socket close.
pub fn spawn_session_reaper(session: &Session) -> (Sessions, tokio::task::JoinHandle<()>) {
    let sessions = Sessions::default();

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

                let expired = {
                    let Ok(mut table) = sessions.lock() else {
                        continue;
                    };
                    table.take_expired(now)
                };

                for (sri, gen_id) in expired {
                    let _ = remove_placement(&session, &sri, gen_id).await;
                    let _ = Mailbox::new(&session).drain_commands(sri).await;
                    tracing::debug!("http session reaped: {sri}");
                }
            }
        }
    });

    (sessions, handle)
}

/// Records a brand-new HTTP session with one attached stream, minted on
/// `mount` of `owner`, and returns its token and browser binding; `gen_id` is
/// the generation its placeholder placement was claimed with. The binding is
/// the first live one in `presented`, else a fresh one, picked and stored
/// under one lock.
pub fn register(
    sessions: &Sessions,
    sri: Uuid,
    gen_id: Gen,
    presented: &[String],
    mount: &str,
    owner: Sri,
) -> (Secret, Secret) {
    let token = Secret::random();
    let mut binding = Secret::random();
    if let Ok(mut table) = sessions.lock() {
        binding = table.live_binding(presented).unwrap_or(binding);
        table.insert(
            token.clone(),
            SessionState {
                active_streams: 1,
                deadline: Instant::now() + HTTP_SESSION_GRACE,
                gen_id,
                sri,
                mount: mount.to_owned(),
                owner,
                binding: binding.clone(),
            },
        );
    }

    (token, binding)
}

/// Attaches a stream to the session behind `token` if it accepts this
/// request; returns its SRI and stored token. A refused check changes nothing.
pub fn attach_stream(
    sessions: &Sessions,
    token: &str,
    presented: &[String],
    mount: &str,
    owner: Sri,
) -> Option<(Uuid, Secret)> {
    let mut table = sessions.lock().ok()?;
    let key = table.map.get_key_value(token)?.0.clone();
    let state = table.accepted(token, presented, mount, owner)?;
    state.active_streams += 1;
    state.deadline = Instant::now() + HTTP_SESSION_GRACE;

    Some((state.sri, key))
}

/// Refreshes the deadline of the session behind `token` if it accepts this
/// request; returns its SRI. A refused check changes nothing.
pub fn touch(
    sessions: &Sessions,
    token: &str,
    presented: &[String],
    mount: &str,
    owner: Sri,
) -> Option<Uuid> {
    let mut table = sessions.lock().ok()?;
    let state = table.accepted(token, presented, mount, owner)?;
    state.deadline = Instant::now() + HTTP_SESSION_GRACE;

    Some(state.sri)
}

/// Compares without an early exit on the first differing byte.
fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use std::slice;
    use std::time::Duration;

    use anyhow::anyhow;
    use serde_json::Value;

    use super::*;

    #[test]
    fn random_shape() {
        let secret = Secret::random();

        assert_eq!(secret.expose().len(), 43);
        assert!(
            secret
                .expose()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
        assert!(Uuid::parse_str(secret.expose()).is_err());
        assert_ne!(secret, Secret::random());
    }

    #[test]
    fn secret_debug_redacts() {
        let secret = Secret::random();
        let debug = format!("{secret:?}");

        assert_eq!(debug, "Secret(..)");
        assert!(!debug.contains(secret.expose()));
    }

    #[test]
    fn secret_map_lookup_by_str() {
        let secret = Secret::random();
        let map = HashMap::from([(secret.clone(), 7)]);

        assert_eq!(map.get(secret.expose()), Some(&7));
        assert_eq!(map.get("not-a-token"), None);
    }

    #[test]
    fn secret_serializes_as_plain_string() -> Result<(), serde_json::Error> {
        let secret = Secret::random();

        assert_eq!(
            serde_json::to_value(&secret)?,
            Value::String(secret.expose().to_owned())
        );

        Ok(())
    }

    #[test]
    fn reapable_predicate() -> anyhow::Result<()> {
        let owner = owner()?;
        let now = Instant::now();
        let grace = Duration::from_secs(1);

        // No streams and the deadline has passed → reap.
        assert!(state(0, now, "/chat", owner).is_reapable(now));
        // A live stream is never reaped, even past the deadline.
        assert!(!state(1, now, "/chat", owner).is_reapable(now));
        // No streams but still inside the grace window → keep.
        assert!(!state(0, now + grace, "/chat", owner).is_reapable(now));

        Ok(())
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
        assert!(ct_eq("", ""));
    }

    #[test]
    fn accepts_only_its_binding_mount_and_owner() -> anyhow::Result<()> {
        let owner = owner()?;
        let other = Sri::of_path("someone-else").map_err(|err| anyhow!("{err}"))?;
        let state = state(0, Instant::now(), "/chat", owner);
        let binding = state.binding.expose().to_owned();

        assert!(state.accepts(slice::from_ref(&binding), "/chat", owner));
        assert!(state.accepts(&["x".to_owned(), binding.clone()], "/chat", owner));
        assert!(!state.accepts(&["x".to_owned()], "/chat", owner));
        assert!(!state.accepts(&[], "/chat", owner));
        assert!(!state.accepts(slice::from_ref(&binding), "/secure", owner));
        assert!(!state.accepts(&[binding], "/chat", other));

        Ok(())
    }

    #[test]
    fn take_expired_returns_sri_and_gen() -> anyhow::Result<()> {
        let owner = owner()?;
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let expired = state(0, now, "/chat", owner);
        let expired_sri = expired.sri;
        let mut table = SessionTable::default();
        table.insert(Secret::random(), expired);
        table.insert(Secret::random(), state(1, now, "/chat", owner));
        table.insert(Secret::random(), state(0, later, "/chat", owner));

        assert_eq!(
            table.take_expired(now),
            vec![(Sri::from_uuid(expired_sri), Gen::from_parts(1, 1))]
        );
        assert_eq!(table.map.len(), 2);

        Ok(())
    }

    #[test]
    fn register_takes_the_first_live_binding() -> anyhow::Result<()> {
        let owner = owner()?;
        let sessions = Sessions::default();
        let mint = |presented: &[String]| {
            register(
                &sessions,
                Uuid::new_v4(),
                Gen::from_parts(1, 1),
                presented,
                "/chat",
                owner,
            )
        };
        let (_, a) = mint(&[]);
        let (_, b) = mint(&[]);
        let value = |secret: &Secret| secret.expose().to_owned();
        let dead = "dead".to_owned();

        assert_ne!(a, b);
        assert_eq!(mint(&[dead.clone(), value(&b), value(&a)]).1, b);
        assert_eq!(mint(&[value(&a), value(&b)]).1, a);
        let (_, fresh) = mint(&[dead]);
        assert_ne!(fresh, a);
        assert_ne!(fresh, b);
        assert_ne!(mint(&[]).1, fresh);

        Ok(())
    }

    #[test]
    fn replaced_token_releases_its_binding() -> anyhow::Result<()> {
        let owner = owner()?;
        let token = Secret::random();
        let (first, second) = (
            state(1, Instant::now(), "/chat", owner),
            state(1, Instant::now(), "/chat", owner),
        );
        let (first_binding, second_binding) = (first.binding.clone(), second.binding.clone());
        let mut table = SessionTable::default();
        table.insert(token.clone(), first);
        table.insert(token, second);

        assert_eq!(table.bindings.get(&first_binding), None);
        assert_eq!(table.bindings.get(&second_binding), Some(&1));

        Ok(())
    }

    #[test]
    fn binding_index_follows_register_and_expiry() -> anyhow::Result<()> {
        let owner = owner()?;
        let sessions = Sessions::default();
        let mint = |presented: &[String]| {
            let gen_id = Gen::from_parts(1, 1);

            register(&sessions, Uuid::new_v4(), gen_id, presented, "/chat", owner)
        };
        let (first, shared) = mint(&[]);
        let (second, same) = mint(&[shared.expose().to_owned()]);
        let (_, other) = mint(&[]);
        assert_eq!(same, shared);
        let count = |binding: &Secret| {
            sessions
                .lock()
                .map(|table| table.bindings.get(binding).copied())
                .map_err(|err| anyhow!("{err}"))
        };
        let live = |binding: &Secret| {
            sessions
                .lock()
                .map(|table| table.live_binding(&[binding.expose().to_owned()]))
                .map_err(|err| anyhow!("{err}"))
        };
        let reap = |token: &Secret| {
            let mut table = sessions.lock().map_err(|err| anyhow!("{err}"))?;
            table.release_stream(token);
            let past_grace = Instant::now() + HTTP_SESSION_GRACE + Duration::from_secs(1);

            anyhow::Ok(table.take_expired(past_grace).len())
        };

        assert_eq!(count(&shared)?, Some(2));
        assert_eq!(count(&other)?, Some(1));

        // One of two sessions sharing the binding goes away: the binding stays.
        assert_eq!(reap(&first)?, 1);
        assert_eq!(count(&shared)?, Some(1));
        assert_eq!(live(&shared)?, Some(shared.clone()));

        // The last one goes away: the binding is dead and its entry is gone.
        assert_eq!(reap(&second)?, 1);
        assert_eq!(count(&shared)?, None);
        assert_eq!(live(&shared)?, None);
        assert_eq!(live(&other)?, Some(other.clone()));

        Ok(())
    }

    fn owner() -> anyhow::Result<Sri> {
        Sri::of_path("chat-owner").map_err(|err| anyhow!("{err}"))
    }

    fn state(active_streams: u32, deadline: Instant, mount: &str, owner: Sri) -> SessionState {
        SessionState {
            active_streams,
            deadline,
            gen_id: Gen::from_parts(1, 1),
            sri: Uuid::new_v4(),
            mount: mount.to_owned(),
            owner,
            binding: Secret::random(),
        }
    }
}
