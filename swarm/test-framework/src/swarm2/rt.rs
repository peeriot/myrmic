//! The handles of a swarm's runtimes: [`StoppedRt`] and [`RunningRt`], the two states of one
//! runtime of a [`Swarm`], as [`crate::runtime`] describes them.
//!
//! # Using them
//!
//! [`Swarm::start`] and [`Swarm::new`] hand them out, one per runtime of the spec, in the order of
//! its variants, so an array pattern names them. They are the only way to get one, so a test
//! never holds two handles of the same runtime. A test moves each through its states by
//! rebinding; starting one returns once the swarm, with it, is connected again, and
//! [`StoppedRt::start_cut_off`] starts one that reaches nobody.
//!
//! A running handle derefs to the backend's own running handle, so everything that doesn't stop
//! the runtime is the backend kind's own method (`a.pause()`). Stopping it in a way only its kind
//! knows goes through [`RunningRt::stop_with`] (`let b = b.stop_with(|rt| rt.kill()).await?;`).
//!
//! A test that picks runtimes by an id it computes, e.g. the host of a cell, turns the array into
//! a map by id ([`IntoRuntimeMap`]). [`start_together`], and [`crate::start_runtimes!`] around it,
//! start several stopped ones side by side and wait once for the swarm.
//!
//! # Design notes
//!
//! The handles wrap the backend's own ones because only they can keep the swarm's record of which
//! runtimes the test has running ([`super::RuntimeEntry`]): a start sets it, and the running
//! handle's guard clears it when the handle goes away, however that happens. The record is the
//! test's intent, which the swarm's connectivity check compares with the runtimes that answer.
//!
//! Every transition that consumes a running runtime ends with it stopped, so one generic
//! [`RunningRt::stop_with`] covers all of them, whatever the kind can do, and the swarm needs to
//! know nothing about the kinds beyond [`crate::runtime`]'s traits. The backend's handle is only
//! lent out for the length of that transition: handing it out for good would leave the swarm's
//! record wrong whichever way the guard went. A method that doesn't consume the handle (e.g.
//! `resume`) is reached through `Deref`, so it doesn't wait for the swarm; a test that needs the
//! swarm connected again calls [`Swarm::wait_until_connected`].
//!
//! A stopped handle holds no backend handle: starting it asks the backend for the runtime of its
//! id anew. That keeps a runtime that ran cut off from coming back cut off after a normal stop.

use std::{
    collections::HashMap,
    future::Future,
    hash::Hash,
    ops::Deref,
    sync::{Arc, atomic::Ordering},
};

use enum_map::{Array, Enum, EnumMap};
use zenoh::config::ZenohId;

#[cfg(doc)]
use super::Swarm;
use super::{CutOff, RtError, Shared, SwarmBackend, SwarmSpec};
use crate::runtime::{Running, Stopped};

/// The running state of the backend's runtimes.
type BackendRunning<B, S> = <<B as SwarmBackend<S>>::Runtime as Stopped>::Running;

/// A runtime of a [`Swarm`] that is not running: never started yet, or stopped. Starting it
/// starts the backend's runtime of the same id (see [`SwarmBackend::runtime`]).
pub struct StoppedRt<B, S> {
    pub(super) id: S,
    pub(super) shared: Arc<Shared<B, S>>,
}

impl<B, S> StoppedRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    /// Start `runtime`, the backend's runtime of this id, and record it as running. No
    /// connectivity check.
    pub(super) async fn start_on(
        self,
        runtime: B::Runtime,
    ) -> Result<RunningRt<B, S>, RtError<B, S>> {
        let inner = runtime.start().await?;
        let id = self.id;
        let entry = self.shared.entry(id);
        let zid = *entry.zid.get_or_init(|| inner.get_zid());
        assert_eq!(
            zid,
            inner.get_zid(),
            "{id:?} came back under a different zid"
        );
        // a swarm hands out one handle per runtime, so its start never finds it running
        assert!(!entry.is_running(), "{id:?} was started while running");
        entry.running.store(true, Ordering::SeqCst);
        Ok(RunningRt {
            inner,
            guard: RunningGuard { stopped: self },
        })
    }
}

impl<B, S> StoppedRt<B, S>
where
    B: CutOff<S>,
    S: SwarmSpec,
{
    /// Start the runtime with its identity and database, but cut off from the rest of the swarm
    /// (see [`CutOff`]). Returns once it is ready, with no connectivity check: it reaches no other
    /// runtime. Stop it before starting it normally to bring it back into the swarm.
    pub async fn start_cut_off(self) -> Result<RunningRt<B, S>, RtError<B, S>> {
        let runtime = self.shared.backend.cut_off_runtime(self.id);
        self.start_on(runtime).await
    }
}

impl<B, S> Stopped for StoppedRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    type Error = RtError<B, S>;

    type Running = RunningRt<B, S>;

    /// Returns once the swarm, with this runtime, is connected again.
    async fn start(self) -> Result<RunningRt<B, S>, RtError<B, S>> {
        let runtime = self.shared.backend.runtime(self.id);
        let running = self.start_on(runtime).await?;
        running.guard.stopped.shared.wait_until_connected().await?;
        Ok(running)
    }
}

/// A running runtime of a [`Swarm`]. Dropping it deletes the runtime. It derefs to the backend's
/// running handle, for the methods of the backend's runtime kind.
pub struct RunningRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    // Declared before `guard`, so the runtime is gone before the swarm stops counting it as
    // running.
    inner: BackendRunning<B, S>,
    guard: RunningGuard<B, S>,
}

impl<B, S> RunningRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    /// Stop the runtime with `stop`, which gets the backend's running handle and returns its
    /// stopped one, e.g. `rt.stop_with(|rt| rt.kill())` or, for a runtime that went
    /// away on its own, `rt.stop_with(|rt| std::future::ready(Ok(rt.mark_lost())))`. Returns
    /// once `stop` has; it does not wait for the rest of the swarm to notice.
    pub async fn stop_with<F, Fut>(self, stop: F) -> Result<StoppedRt<B, S>, RtError<B, S>>
    where
        F: FnOnce(BackendRunning<B, S>) -> Fut,
        Fut: Future<Output = Result<B::Runtime, RtError<B, S>>>,
    {
        let RunningRt { inner, guard } = self;
        // The backend's stopped handle is dropped: starting the swarm's one asks the backend anew.
        stop(inner).await?;
        Ok(guard.into_stopped())
    }
}

/// The stopped handle of a running runtime, which records the runtime as running in its swarm
/// while it exists.
struct RunningGuard<B, S>
where
    S: SwarmSpec,
{
    stopped: StoppedRt<B, S>,
}

impl<B, S> RunningGuard<B, S>
where
    S: SwarmSpec,
{
    /// The stopped handle of the runtime, now no longer recorded as running. A copy, since `Drop`
    /// forbids moving it out; the guard is gone right after, so there is still one handle per
    /// runtime. The backend's own stopped handle is not kept: starting the result describes the
    /// runtime anew from the backend.
    fn into_stopped(self) -> StoppedRt<B, S> {
        StoppedRt {
            id: self.stopped.id,
            shared: self.stopped.shared.clone(),
        }
    }
}

impl<B, S> Drop for RunningGuard<B, S>
where
    S: SwarmSpec,
{
    fn drop(&mut self) {
        let StoppedRt { id, shared } = &self.stopped;
        shared.entry(*id).running.store(false, Ordering::SeqCst);
    }
}

impl<B, S> Running for RunningRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    type Error = RtError<B, S>;

    type Stopped = StoppedRt<B, S>;

    fn get_zid(&self) -> ZenohId {
        self.inner.get_zid()
    }

    async fn delete(self) -> Result<StoppedRt<B, S>, RtError<B, S>> {
        self.stop_with(|rt| rt.delete()).await
    }
}

impl<B, S> Deref for RunningRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    type Target = BackendRunning<B, S>;

    fn deref(&self) -> &BackendRunning<B, S> {
        &self.inner
    }
}

/// A handle of one of a swarm's runtimes, which knows which runtime of the spec it is.
pub trait SwarmRt {
    /// the spec the swarm's runtimes come from
    type Id: SwarmSpec;

    /// which runtime of the spec this is
    fn get_id(&self) -> Self::Id;
}

impl<B, S> SwarmRt for StoppedRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    type Id = S;

    fn get_id(&self) -> S {
        self.id
    }
}

impl<B, S> SwarmRt for RunningRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    type Id = S;

    fn get_id(&self) -> S {
        self.guard.stopped.id
    }
}

/// Turns a swarm's array of runtime handles (see [`Swarm::start`]) into a map by runtime id, for
/// tests that pick runtimes by an id they compute.
pub trait IntoRuntimeMap {
    /// the spec the swarm's runtimes come from
    type Id;

    /// the handles' type
    type Rt;

    /// the handles by id, to borrow one: `rts[host].get_zid()`
    fn into_enum_map(self) -> EnumMap<Self::Id, Self::Rt>
    where
        Self::Id: Enum;

    /// the handles by id, to take one out for a transition that consumes it:
    /// `rts.remove(&host).expect("host is running").stop_with(|rt| rt.kill())`
    fn into_map(self) -> HashMap<Self::Id, Self::Rt>
    where
        Self::Id: Hash + Eq;
}

impl<R, const N: usize> IntoRuntimeMap for [R; N]
where
    R: SwarmRt,
{
    type Id = R::Id;

    type Rt = R;

    fn into_enum_map(self) -> EnumMap<R::Id, R> {
        let mut slots: Vec<Option<R>> = (0..<R::Id as Enum>::Array::<()>::LENGTH)
            .map(|_| None)
            .collect();
        for rt in self {
            let id = rt.get_id();
            let slot = &mut slots[id.into_usize()];
            assert!(slot.is_none(), "two handles of {id:?}");
            *slot = Some(rt);
        }
        EnumMap::from_fn(|id: R::Id| {
            slots[id.into_usize()]
                .take()
                .unwrap_or_else(|| panic!("no handle of {id:?}"))
        })
    }

    fn into_map(self) -> HashMap<R::Id, R>
    where
        R::Id: Hash + Eq,
    {
        self.into_iter().map(|rt| (rt.get_id(), rt)).collect()
    }
}

/// Start the stopped runtimes `stopped`, all of one swarm, side by side, then wait once until the
/// swarm, with all of them, is connected. Returns the running handles in the order of `stopped`.
///
/// Starting them one by one would check the swarm after each start, against a set of runtimes that
/// is still growing. [`crate::start_runtimes!`] wraps it to rebind each name to its running handle.
pub async fn start_together<B, S>(
    stopped: impl IntoIterator<Item = StoppedRt<B, S>>,
) -> Result<Vec<RunningRt<B, S>>, RtError<B, S>>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
{
    let stopped: Vec<StoppedRt<B, S>> = stopped.into_iter().collect();
    let shared = stopped
        .first()
        .expect("there is at least one runtime to start")
        .shared
        .clone();
    assert!(
        stopped.iter().all(|rt| Arc::ptr_eq(&rt.shared, &shared)),
        "the runtimes to start belong to different swarms"
    );
    // `join_all` keeps the order of its futures
    let started = futures::future::join_all(stopped.into_iter().map(|rt| {
        let runtime = rt.shared.backend.runtime(rt.id);
        rt.start_on(runtime)
    }))
    .await;
    let running = started.into_iter().collect::<Result<Vec<_>, _>>()?;
    shared.wait_until_connected().await?;
    Ok(running)
}

/// Start the stopped swarm runtimes `a`, `b`, ... side by side and rebind each name to its running
/// handle, returning the first error with `?`. Returns once the swarm, with all of them, is
/// connected (see [`start_together`]).
#[macro_export]
macro_rules! start_runtimes {
    ($($runtime:ident),+ $(,)?) => {
        // the pattern gives the array its length
        let [$($runtime),+] = $crate::swarm2::start_together([$($runtime),+])
            .await?
            .try_into()
            .unwrap_or_else(|running: Vec<_>| {
                panic!("{} runtimes came back from their starts", running.len())
            });
    };
}
