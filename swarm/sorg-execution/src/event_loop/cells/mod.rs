use sorg_common::PoisonSnd;
use tokio::task::JoinHandle;

mod deploy;
mod undeploy;

/// Per-cell handle held by the exec event loop while the cell is running.
///
/// Dropping the handle closes the poison channel, which terminates the
/// cell task and all the subscriptions / queryables it owns. The watcher
/// task owns the cell task's join handle and reports its exit to the event
/// loop as `Event::CellExited`.
pub(crate) struct CellHandle {
    _poison: PoisonSnd,
    _watcher: JoinHandle<()>,
}

impl CellHandle {
    pub(crate) fn new(poison: PoisonSnd, watcher: JoinHandle<()>) -> Self {
        Self {
            _poison: poison,
            _watcher: watcher,
        }
    }
}
