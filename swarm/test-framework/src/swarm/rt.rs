//! The handles of a swarm's runtimes: [`StoppedRt`] and [`RunningRt`], the two states of one
//! runtime of a [`Swarm`], as [`crate::runtime`] describes them, around the handle of the
//! runtime's own kind.
//!
//! # Using them
//!
//! [`Swarm::add`] turns a stopped runtime of any kind into the swarm's stopped handle for it. A
//! test moves each through its states by rebinding; starting one returns once the swarm, with it,
//! is connected again, and [`StoppedRt::start_without_check`] starts one without that check.
//! [`crate::start_runtimes!`] starts several side by side and checks once.
//!
//! A running handle derefs to the kind's own running handle, so everything that doesn't stop the
//! runtime is the kind's own method (`a.pause()`). Stopping it in a way only its kind knows goes
//! through [`RunningRt::stop_with`] (`let b = b.stop_with(|rt| rt.kill()).await?;`).
//!
//! # Design notes
//!
//! The handles wrap the kind's own ones because only they can keep the swarm's record of which
//! runtimes the test has running: a start sets it, and the running handle's guard clears it when
//! the handle goes away, however that happens. The record is the test's intent, which the swarm's
//! connectivity check compares with the runtimes that answer.
//!
//! Every transition that consumes a running runtime ends with it stopped, so one generic
//! [`RunningRt::stop_with`] covers all of them, whatever the kind can do, and the swarm needs to
//! know nothing about the kinds beyond [`crate::runtime`]'s traits. The kind's handle is only lent
//! out for the length of that transition: handing it out for good would leave the swarm's record
//! wrong whichever way the guard went. A method that doesn't consume the handle (e.g. `resume`) is
//! reached through `Deref`, so it doesn't wait for the swarm; a test that needs the swarm
//! connected again calls [`Swarm::wait_until_connected`].

use std::{
    future::Future,
    ops::Deref,
    sync::{Arc, atomic::Ordering},
};

use zenoh::config::ZenohId;

#[cfg(doc)]
use super::Swarm;
use super::{RuntimeEntry, Shared};
use crate::{
    myrmic, overlay,
    runtime::{Running, Stopped},
};

/// A runtime of a [`Swarm`] that is not running: never started yet, or stopped. It holds the
/// stopped handle of the runtime's own kind `S`.
pub struct StoppedRt<S> {
    inner: S,
    entry: Arc<RuntimeEntry>,
    shared: Arc<Shared>,
}

impl<S> StoppedRt<S>
where
    S: Stopped,
{
    /// the swarm's handle for `inner`, recorded in the swarm as `entry`
    pub(super) fn new(inner: S, entry: Arc<RuntimeEntry>, shared: Arc<Shared>) -> Self {
        Self {
            inner,
            entry,
            shared,
        }
    }

    /// Start the runtime and record it as running, without waiting until the swarm is connected:
    /// returns once the runtime is ready, as its kind's start does, with none of the swarm's
    /// guarantees about who it is linked to.
    pub async fn start_without_check(self) -> Result<RunningRt<S::Running>, S::Error> {
        let StoppedRt {
            inner,
            entry,
            shared,
        } = self;
        let inner = inner.start().await?;
        let zid = *entry.zid.get_or_init(|| inner.get_zid());
        assert_eq!(
            zid,
            inner.get_zid(),
            "{:?} came back under a different zid",
            entry.member
        );
        // a runtime's handle moved into the swarm's, so its start never finds it running
        assert!(
            !entry.running.load(Ordering::SeqCst),
            "{:?} was started while running",
            entry.member
        );
        entry.running.store(true, Ordering::SeqCst);
        Ok(RunningRt {
            inner,
            guard: RunningGuard { entry, shared },
        })
    }
}

impl<S> Stopped for StoppedRt<S>
where
    S: Stopped<Error: From<myrmic::Error>>,
{
    type Error = S::Error;

    type Running = RunningRt<S::Running>;

    /// Returns once the swarm, with this runtime, is connected again.
    async fn start(self) -> Result<RunningRt<S::Running>, S::Error> {
        let running = self.start_without_check().await?;
        running.guard.shared.wait_until_connected().await?;
        Ok(running)
    }

    fn get_overlay_member(&self) -> &overlay::Member {
        self.inner.get_overlay_member()
    }
}

/// A running runtime of a [`Swarm`], around the running handle of the runtime's own kind `R`,
/// which it derefs to. Dropping it drops that handle, which deletes the runtime.
pub struct RunningRt<R> {
    // Declared before `guard`, so the runtime is gone before the swarm stops counting it as
    // running.
    inner: R,
    guard: RunningGuard,
}

/// What records a running runtime as running in its swarm, for as long as it exists.
struct RunningGuard {
    entry: Arc<RuntimeEntry>,
    shared: Arc<Shared>,
}

impl RunningGuard {
    /// The swarm's stopped handle around `inner`, the runtime no longer recorded as running. The
    /// guard is gone right after, so there is still one handle per runtime.
    fn into_stopped<S>(self, inner: S) -> StoppedRt<S> {
        StoppedRt {
            inner,
            entry: self.entry.clone(),
            shared: self.shared.clone(),
        }
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.entry.running.store(false, Ordering::SeqCst);
    }
}

impl<R> RunningRt<R>
where
    R: Running,
{
    /// Stop the runtime with `stop`, which gets the kind's running handle and returns its stopped
    /// one, e.g. `rt.stop_with(|rt| rt.kill())` or, for a runtime that went away on its own,
    /// `rt.stop_with(|rt| std::future::ready(Ok(rt.mark_lost())))`. Returns once `stop` has; it
    /// does not wait for the rest of the swarm to notice.
    pub async fn stop_with<F, Fut>(self, stop: F) -> Result<StoppedRt<R::Stopped>, R::Error>
    where
        F: FnOnce(R) -> Fut,
        Fut: Future<Output = Result<R::Stopped, R::Error>>,
    {
        let RunningRt { inner, guard } = self;
        let stopped = stop(inner).await?;
        Ok(guard.into_stopped(stopped))
    }
}

impl<R> Running for RunningRt<R>
where
    R: Running<Error: From<myrmic::Error>>,
{
    type Error = R::Error;

    type Stopped = StoppedRt<R::Stopped>;

    fn get_zid(&self) -> ZenohId {
        self.inner.get_zid()
    }

    async fn delete(self) -> Result<StoppedRt<R::Stopped>, R::Error> {
        self.stop_with(|rt| rt.delete()).await
    }
}

impl<R> Deref for RunningRt<R> {
    type Target = R;

    fn deref(&self) -> &R {
        &self.inner
    }
}

/// Start the stopped swarm runtimes `a`, `b`, ... of `swarm` side by side, rebind each name to its
/// running handle, and wait once until the swarm, with all of them, is connected; returns the
/// first error with `?`. The runtimes may be of different kinds.
///
/// Starting them one by one would check the swarm after each start, against a set of runtimes that
/// is still growing.
#[macro_export]
macro_rules! start_runtimes {
    ($swarm:expr; $($runtime:ident),+ $(,)?) => {
        let ($($runtime,)+) = $crate::__futures::join!($($runtime.start_without_check()),+);
        $(let $runtime = $runtime?;)+
        $swarm.wait_until_connected().await?;
    };
}
