//! A runtime a test controls, as two states that are each a type of their own: [`Stopped`] can
//! only be started, [`Running`] can only be stopped. Each transition consumes the value it starts
//! from and returns the other state, so calling one on a runtime in the wrong state does not
//! compile.
//!
//! What only some kinds of runtimes can do is an extra trait on the running state: [`Kill`],
//! [`Pause`], [`Lose`].
//!
//! # Using a runtime
//!
//! A test gets a stopped runtime from whatever describes it, e.g.
//! [`crate::myrmic::Myrmic::runtime`] for one started with `myrmic runtimes start`, and moves it
//! through its states by rebinding:
//!
//! ```ignore
//! use test_framework::myrmic::Myrmic;
//! use test_framework::runtime::{Kill, Running, Stopped};
//!
//! let myrmic = Myrmic::local_isolated();
//! let rt = myrmic.runtime(&["gpu"]).persistent(true).start().await?; // running
//! let zid = rt.get_zid();
//! let rt = rt.kill().await?; // stopped
//! let rt = rt.start().await?; // running again: the same runtime, so the same zid and data
//! assert_eq!(rt.get_zid(), zid);
//! ```
//!
//! - The transitions are trait methods: import the traits a test calls ([`Stopped`] to start,
//!   [`Running`] to delete or read the zid, [`Kill`], [`Pause`], [`Lose`]).
//! - [`Stopped::start`] returns once the runtime is ready, i.e. once cells can be placed on it.
//! - A stopped runtime is the same runtime it was while running: starting it again brings it back
//!   under the same identity. With the CLI, the identity is the name: the same name gives the
//!   same zid, and with `persistent` the same database. A stopped value can still be changed
//!   before it starts again (e.g. switched to `persistent`), but giving it another name makes it
//!   a different runtime.
//! - Dropping a running runtime deletes it, best effort, so a failing test leaves nothing running.
//!   Dropping a stopped runtime does nothing. A test deletes explicitly only when it asserts on
//!   what happens after the delete.
//! - A failed transition consumes the value too: the runtime's handle is gone with the error. A
//!   test treats every such error as a failure (`.unwrap()`, `?`), so there is nothing to
//!   continue with anyway.
//! - Code that needs a capability states it as a bound (`R: Kill`); a runtime kind without it is
//!   rejected at compile time, not at run time.
//!
//! These traits are not tied to swarms: a test with a single runtime uses them directly, and a
//! swarm's runtimes are the same handles.
//!
//! # Design notes
//!
//! Why two types instead of one handle with a running flag: every operation is only valid in one
//! state, and a flag would turn each wrong-state call into a run-time assertion. With the states
//! as types, the compiler rejects them. [`Running::get_zid`] is the clearest case: a runtime that
//! was never started has no zid, so the getter exists only on the running state, and no caller
//! has to handle its absence.
//!
//! Each implementation pairs exactly one stopped type with one running type, which the associated
//! types [`Stopped::Running`] and [`Running::Stopped`] tie together, so a round trip always comes
//! back to the type it started from.
//!
//! The core traits hold only what every kind of runtime can do: start, delete, and tell its zid.
//! Anything a kind may not be able to do is a capability trait instead, the same way the swarm
//! backends' faults are (see [`crate::swarm2::backend`]). For example, a runtime whose node runs
//! inside the test process could not be ended abruptly (closing its zenoh session always says
//! goodbye) or frozen (there is no process of its own to stop), so it would implement neither
//! [`Kill`] nor [`Pause`].
//!
//! A stopped value holds everything starting needs, so a running value has to hold it as well, to
//! hand it back when it stops. For the CLI, the stopped type is the set of `runtimes start`
//! options itself ([`crate::myrmic::StoppedRt`]), and the running type keeps it next to the zid
//! ([`crate::myrmic::RunningRt`]).
//!
//! Handles are `Send + Sync`: tests hold them across awaits on a multi-threaded runtime, and a
//! swarm's handles share its state with the swarm (see [`crate::swarm2`]).
//!
//! Each kind reports failures in its own error type ([`Stopped::Error`], shared by both states of
//! a kind): a CLI runtime fails with what the CLI said ([`crate::myrmic::Error`]).

use std::future::Future;

use zenoh::config::ZenohId;

/// A runtime that is not running: never started yet, or stopped.
pub trait Stopped: Sized + Send + Sync {
    /// what a failed transition of this kind of runtime reports, in either state
    type Error;

    /// the state [`Self::start`] returns
    type Running: Running<Stopped = Self, Error = Self::Error>;

    /// Start the runtime. Returns once it is ready.
    fn start(self) -> impl Future<Output = Result<Self::Running, Self::Error>> + Send;
}

/// A running runtime.
pub trait Running: Sized + Send + Sync {
    /// the same as [`Stopped::Error`]
    type Error;

    /// the state [`Self::delete`] returns
    type Stopped: Stopped<Running = Self, Error = Self::Error>;

    /// the zenoh id the runtime runs under
    fn get_zid(&self) -> ZenohId;

    /// Shut the runtime down gracefully. Returns once it is gone.
    fn delete(self) -> impl Future<Output = Result<Self::Stopped, Self::Error>> + Send;
}

/// A running runtime that can be ended abruptly.
pub trait Kill: Running {
    /// End the runtime as on a crash or power loss: it neither says goodbye to the swarm nor
    /// cleans up. Returns once it is gone.
    fn kill(self) -> impl Future<Output = Result<Self::Stopped, Self::Error>> + Send;
}

/// A running runtime that can disappear without the test stopping it, e.g. with its host's power.
pub trait Lose: Running {
    /// Turn the handle of a runtime that is already gone into a stopped one, without contacting
    /// the runtime or its host (the host may still be down). Starting the result starts the same
    /// runtime again.
    fn mark_lost(self) -> Self::Stopped;
}

/// A running runtime that can be frozen and let run again.
pub trait Pause: Running {
    /// Freeze the runtime, as a stalled machine would: it keeps its state and connections but
    /// does nothing, timers and lease renewals included, until [`Self::resume`].
    fn pause(&self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// let the runtime [`Self::pause`] froze run again
    fn resume(&self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
