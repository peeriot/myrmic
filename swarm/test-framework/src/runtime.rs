//! A runtime a test controls, as two states that are each a type of their own: a [`Stopped`]
//! runtime can only be started, a [`Running`] one can only be stopped. Each transition consumes
//! the value it starts from and returns the other state. So calling a transition on a runtime in
//! the wrong state doesn't compile.
//!
//! What only some kinds of runtimes can do (kill, pause, ...) is a method of that kind's running
//! type, not part of these traits.
//!
//! # Using a runtime
//!
//! A test gets a stopped runtime from whatever describes it, e.g.
//! [`crate::myrmic::Myrmic::runtime`] for a runtime of `myrmic runtimes start`. It moves the
//! runtime through its states by rebinding:
//!
//! ```ignore
//! use test_framework::{
//!     World, host,
//!     overlay::Overlay,
//!     runtime::{Running, Stopped},
//! };
//!
//! let group = Overlay::new(&World::local()).new_multicast_group();
//! let myrmic = host::Local.myrmic_isolated(&group);
//! let rt = myrmic.runtime().tags(&["gpu"]).persistent(true).start().await?; // running
//! let zid = rt.get_zid();
//! let rt = rt.kill().await?; // stopped
//! let rt = rt.start().await?; // running again: the same runtime, so the same zid and data
//! assert_eq!(rt.get_zid(), zid);
//! ```
//!
//! - The transitions every kind has are trait methods, so a test imports the traits it calls:
//!   [`Stopped`] to start, [`Running`] to delete or to read the zid. All other transitions are
//!   methods of the kind (e.g. [`crate::myrmic::RunningRt::kill`]).
//! - [`Stopped::start`] returns once the runtime is ready, i.e. once cells can be placed on it.
//! - A stopped runtime is the same runtime it was while running: starting it again brings it back
//!   under the same identity. For a CLI runtime, the identity is its name. The same name gives
//!   the same zid, and with `persistent` also the same database. A test can still change a kind's
//!   stopped value before starting it again (e.g. switch it to `persistent`), but giving it
//!   another name makes it a different runtime. A swarm's stopped handle can't be changed: it
//!   doesn't hand out the kind's value, and the kinds' setters need to consume the value they
//!   change.
//! - Dropping a running runtime deletes it, best effort, so a failing test leaves nothing running.
//!   Dropping a stopped runtime does nothing. A test only deletes a runtime explicitly when it
//!   checks what happens after the delete.
//! - A failed transition also consumes the value, so the handle is gone together with the error.
//!   A test treats every such error as a failure (`.unwrap()`, `?`), so there is nothing to
//!   continue with anyway.
//! - Calling a method that a kind doesn't have doesn't compile. A test names the concrete kinds it
//!   uses, so nothing needs to be checked at run time.
//!
//! These traits are not tied to swarms. A test with a single runtime uses them directly, and the
//! handles of a swarm implement them by wrapping the handles of the kind.
//!
//! # Design notes
//!
//! Why two types instead of one handle with a running flag: every operation is only valid in one
//! of the states, and a flag would turn each call in the wrong state into a check at run time.
//! With the states as types, the compiler rejects those calls. [`Running::get_zid`] is the
//! clearest case: a runtime that was never started has no zid, so the getter only exists on the
//! running state, and no caller has to handle a missing zid.
//!
//! Each implementation pairs exactly one stopped type with one running type. The associated types
//! [`Stopped::Running`] and [`Running::Stopped`] tie the two together, so a round trip always
//! comes back to the type it started from.
//!
//! The traits only hold what every kind of runtime can do: start, delete, and report its zid and
//! its overlay membership. Code that is generic over the kinds needs no more. A swarm, for example,
//! starts and stops its runtimes through these traits, and gives the test the concrete handle for
//! everything else (see [`crate::swarm::RunningRt::stop_with`]). Anything that some kind may not
//! be able to do is a method of the kinds that can. A runtime whose node runs inside the test
//! process ([`crate::in_process`]), for example, can't be ended abruptly (closing its zenoh
//! session always says goodbye) or frozen (it has no process of its own to stop), so it has
//! neither `kill` nor `pause`.
//!
//! A stopped value holds everything that starting needs. So a running value has to hold it as
//! well, to return it when the runtime stops. For a CLI runtime, the stopped type is the set of
//! `runtimes start` options ([`crate::myrmic::StoppedRt`]), and the running type keeps those
//! options next to the zid ([`crate::myrmic::RunningRt`]).
//!
//! Handles are `Send + Sync`: tests hold them across awaits on a multi-threaded runtime, and the
//! handles of a swarm share its state with the swarm (see [`crate::swarm`]).
//!
//! Each kind reports failures in its own error type ([`Stopped::Error`], which both states of a
//! kind share). A CLI runtime fails with what the CLI said ([`crate::myrmic::Error`]), an
//! in-process node with what its startup reported.

use std::future::Future;

use zenoh::config::ZenohId;

use crate::overlay;

/// A runtime that is not running: never started yet, or stopped.
pub trait Stopped: Sized + Send + Sync {
    /// what a failed transition of this kind of runtime reports, in either state
    type Error;

    /// the state [`Self::start`] returns
    type Running: Running<Stopped = Self, Error = Self::Error>;

    /// Start the runtime. Returns once it is ready.
    fn start(self) -> impl Future<Output = Result<Self::Running, Self::Error>> + Send;

    /// which member of its [`crate::overlay::Overlay`] the runtime is: every runtime is created as
    /// one, and takes its zenoh links from the overlay
    fn get_overlay_member(&self) -> &overlay::Member;
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
