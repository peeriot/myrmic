//! How far this node has got towards being usable by the swarm.
//!
//! Three facts decide it, each reported by the subsystem that owns it: the
//! network service knows whether the radio is associated and whether a zenoh
//! session stands, and the db service knows whether this node's exec-registry
//! row is written. None of them is worth a public surface on its own — what a
//! firmware wants is the one answer they add up to, which is what
//! [`status`] gives it.
//!
//! ```ignore
//! #[esp_firmware::main]
//! async fn setup(board: &mut Board, spawner: Spawner) {
//!     let pin = board.take_pin(8).unwrap();
//!     spawner.spawn(indicate(esp_firmware::status_watch().unwrap(), pin).unwrap());
//! }
//!
//! #[embassy_executor::task]
//! async fn indicate(mut status: StatusWatch, mut pin: Flex<'static>) {
//!     loop {
//!         pin.set_level((esp_firmware::status() == NodeStatus::Registered).into());
//!         status.changed().await;
//!     }
//! }
//! ```

use core::sync::atomic::{AtomicU8, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{DynReceiver, Watch};

/// How many [`StatusWatch`]es may exist at once.
const WATCHERS: usize = 2;

/// The facts, ordered: each depends on the one below it.
const ASSOCIATED: u8 = 1 << 0;
const SESSION: u8 = 1 << 1;
const REGISTERED: u8 = 1 << 2;

static FACTS: AtomicU8 = AtomicU8::new(0);
static STATUS: Watch<CriticalSectionRawMutex, NodeStatus, WATCHERS> =
    Watch::new_with(NodeStatus::Down);

/// What the swarm can do with this node.
///
/// A ladder, not a set of flags: each rung needs the one below it, so a node
/// that loses its radio is [`Down`](Self::Down) whatever else was true a moment
/// ago.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeStatus {
    /// The radio is not associated with an access point.
    Down,
    /// Associated, but with no zenoh session: the swarm is out of reach.
    Associated,
    /// The session stands, but this node is not in the exec registry — it
    /// cannot yet be seen or deployed to.
    Connected,
    /// The exec-registry row is written: the node is visible to `m network`
    /// and is a placement target.
    Registered,
}

/// This node's status, as of now.
#[must_use]
pub fn status() -> NodeStatus {
    STATUS.try_get().unwrap_or(NodeStatus::Down)
}

/// A handle that wakes on every change of [`status`].
///
/// `None` once two of them exist; hold the one you take rather than asking
/// again per wait.
#[must_use]
pub fn status_watch() -> Option<StatusWatch> {
    STATUS.dyn_receiver().map(StatusWatch)
}

/// Wakes its holder whenever this node's [`NodeStatus`] changes.
pub struct StatusWatch(DynReceiver<'static, NodeStatus>);

impl StatusWatch {
    /// Waits for the status to change, and returns what it changed to.
    ///
    /// The first call returns the status as it stands, so a task can paint
    /// before it waits without reading it separately.
    pub async fn changed(&mut self) -> NodeStatus {
        self.0.changed().await
    }
}

/// Reports whether the radio is joined to its access point.
pub(crate) fn set_associated(up: bool) {
    set(ASSOCIATED, up);
}

/// Reports whether a zenoh session stands.
pub(crate) fn set_session(up: bool) {
    set(SESSION, up);
}

/// Reports whether this node's exec-registry row is written.
pub(crate) fn set_registered(up: bool) {
    set(REGISTERED, up);
}

/// Records one fact and publishes what the three now add up to.
///
/// A fact that goes away takes with it every fact above it — an unassociated
/// radio has no session, a dropped session no registration — so a report that
/// arrives late can never leave a higher rung standing on nothing.
fn set(fact: u8, up: bool) {
    let facts = if up {
        FACTS.fetch_or(fact, Ordering::Relaxed) | fact
    } else {
        // Only the facts strictly below this one survive.
        let keep = fact - 1;
        FACTS.fetch_and(keep, Ordering::Relaxed) & keep
    };

    let status = derive(facts);
    STATUS.sender().send_if_modified(|current| {
        if *current == Some(status) {
            false
        } else {
            *current = Some(status);
            true
        }
    });
}

/// The rung the facts reach.
const fn derive(facts: u8) -> NodeStatus {
    if facts & ASSOCIATED == 0 {
        NodeStatus::Down
    } else if facts & SESSION == 0 {
        NodeStatus::Associated
    } else if facts & REGISTERED == 0 {
        NodeStatus::Connected
    } else {
        NodeStatus::Registered
    }
}
