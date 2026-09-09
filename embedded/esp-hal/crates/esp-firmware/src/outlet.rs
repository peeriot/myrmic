//! Outlets: commands the cell writes for the firmware to act on.
//!
//! An outlet is the write-side mirror of a [`Tap`](crate::Tap): a named,
//! typed slot the cell drives through the `outlet` host module and the
//! firmware consumes. The last write wins; there is no queue and no
//! arbitration, because an actuator has exactly one desired state. Declare one
//! on the [`Board`](crate::Board) before `start` runs, then move the handle
//! into the task that owns the hardware it commands.

use alloc::boxed::Box;
use core::fmt;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use serde::de::DeserializeOwned;
use signal_layer_core::{
    AnyWritable, OutletEntry, OutletRegistry, RetainedSlot, TapError, Timestamp, WireType,
};

use crate::tap::DeclareError;

/// A retained slot that also wakes the task waiting on it.
///
/// The cell's write lands on the WAMR thread; the firmware task awaiting it
/// runs on an embassy executor. The critical-section `Signal` is what carries
/// the wake across, exactly as the network service wakes the main executor.
struct OutletSlot<T: Clone + Send + 'static> {
    latest: RetainedSlot<T>,
    written: Signal<CriticalSectionRawMutex, ()>,
}

impl<T> AnyWritable for OutletSlot<T>
where
    T: DeserializeOwned + WireType + Clone + Send + Sync + 'static,
{
    fn write_bytes(&self, ts: Timestamp, bytes: &[u8]) -> Result<(), TapError> {
        AnyWritable::write_bytes(&self.latest, ts, bytes)?;
        self.written.signal(());
        Ok(())
    }

    fn wire_type_id(&self) -> u32 {
        T::TYPE_ID
    }
}

/// A command slot the cell writes and the firmware acts on.
///
/// `Copy`, but with one waiter: [`changed`](Self::changed) wakes the task that
/// awaited it most recently, so give each outlet to one task and let that task
/// own the hardware.
pub struct Outlet<T: Clone + Send + 'static> {
    name: &'static str,
    slot: &'static OutletSlot<T>,
}

impl<T> Outlet<T>
where
    T: DeserializeOwned + WireType + Clone + Send + Sync + 'static,
{
    pub(crate) fn declare(
        registry: &mut OutletRegistry,
        name: &'static str,
    ) -> Result<Self, DeclareError> {
        let slot: &'static OutletSlot<T> = Box::leak(Box::new(OutletSlot {
            latest: RetainedSlot::new(),
            written: Signal::new(),
        }));
        registry.register(name, OutletEntry::retained(slot))?;
        Ok(Self { name, slot })
    }
}

impl<T: Clone + Send + 'static> Outlet<T> {
    /// The name a cell resolves this outlet by.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The most recent command and when the cell wrote it, if it has written
    /// one yet.
    #[must_use]
    pub fn read(&self) -> Option<(Timestamp, T)> {
        self.slot.latest.read()
    }

    /// Waits for the cell to write, then returns the latest command.
    ///
    /// A write that landed before this call and has not been awaited yet
    /// resolves it at once, so a task that starts after the cell has already
    /// spoken still applies that command. Several writes between two awaits
    /// coalesce into one wake carrying the last of them, which is the one the
    /// actuator should be in anyway.
    pub async fn changed(&self) -> (Timestamp, T) {
        loop {
            self.slot.written.wait().await;
            if let Some(latest) = self.slot.latest.read() {
                return latest;
            }
        }
    }
}

#[expect(
    clippy::expl_impl_clone_on_copy,
    reason = "derive(Clone, Copy) would require T: Copy"
)]
impl<T: Clone + Send + 'static> Clone for Outlet<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Clone + Send + 'static> Copy for Outlet<T> {}

impl<T: Clone + Send + 'static> fmt::Debug for Outlet<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outlet")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
