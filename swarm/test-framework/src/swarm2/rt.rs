//! The handles of a swarm's runtimes: [`StoppedRt`] and [`RunningRt`], the two states of one
//! runtime of a [`Swarm`], as [`crate::runtime`] describes them.
//!
//! # Using them
//!
//! [`Swarm::start`] and [`Swarm::new`] hand them out, one per runtime of the spec, in the order of
//! its variants, so an array pattern names them. They are the only way to get one, so a test
//! never holds two handles of the same runtime. A test moves each through its states by
//! rebinding (`let b = b.kill().await?;`); starting one returns once the swarm, with it, is
//! connected again, and [`StoppedRt::start_cut_off`] starts one that reaches nobody.
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
//! A stopped handle holds no backend handle: starting it asks the backend for the runtime of its
//! id anew. That keeps a runtime that ran cut off from coming back cut off after a normal stop.

use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Arc, atomic::Ordering},
};

use enum_map::{Array, Enum, EnumMap};
use zenoh::config::ZenohId;

#[cfg(doc)]
use super::Swarm;
use super::{CutOff, RtError, Shared, SwarmBackend, SwarmSpec};
use crate::runtime::{Kill, Lose, Pause, Running, Stopped};

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

/// A running runtime of a [`Swarm`]. Dropping it deletes the runtime.
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
        let RunningRt { inner, guard } = self;
        inner.delete().await?;
        Ok(guard.into_stopped())
    }
}

impl<B, S> Kill for RunningRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
    BackendRunning<B, S>: Kill,
{
    /// Returns once the runtime's process is gone; it does not wait for the rest of the swarm to
    /// notice.
    async fn kill(self) -> Result<StoppedRt<B, S>, RtError<B, S>> {
        let RunningRt { inner, guard } = self;
        inner.kill().await?;
        Ok(guard.into_stopped())
    }
}

impl<B, S> Lose for RunningRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
    BackendRunning<B, S>: Lose,
{
    fn mark_lost(self) -> StoppedRt<B, S> {
        let RunningRt { inner, guard } = self;
        inner.mark_lost();
        guard.into_stopped()
    }
}

impl<B, S> Pause for RunningRt<B, S>
where
    B: SwarmBackend<S>,
    S: SwarmSpec,
    BackendRunning<B, S>: Pause,
{
    /// The rest of the swarm hears nothing from the runtime until [`Pause::resume`].
    async fn pause(&self) -> Result<(), RtError<B, S>> {
        self.inner.pause().await
    }

    /// Returns once the swarm, with this runtime, is connected again.
    async fn resume(&self) -> Result<(), RtError<B, S>> {
        self.inner.resume().await?;
        self.guard.stopped.shared.wait_until_connected().await?;
        Ok(())
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
    /// `rts.remove(&host).expect("host is running").kill()`
    fn into_map(self) -> HashMap<Self::Id, Self::Rt>
    where
        Self::Id: Hash;
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
        R::Id: Hash,
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
