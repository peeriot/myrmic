//! Taps: values the firmware publishes for the cell to read.
//!
//! A tap is a named slot in the Signal Layer's tap registry. The firmware
//! writes it from its own tasks; a cell resolves the name through the `tap`
//! host module and reads the latest value, or drains the queued events.
//! Declare one on the [`Board`](crate::Board) before `start` runs, then move
//! the handle into whichever task produces the values.
//!
//! The pipeline codegen declares its taps as statics and registers them the
//! same way; a hand-declared tap and a generated one are indistinguishable to
//! the cell.

use alloc::boxed::Box;
use core::fmt;

use serde::Serialize;
use signal_layer_core::{
    EventSlot, RetainedSlot, SlotEntry, TapError, TapRegistry, Timestamp, WireType,
};

/// Why a tap or outlet could not be declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclareError {
    /// The registry is at capacity: [`MAX_TAPS`](signal_layer_core::MAX_TAPS)
    /// taps or [`MAX_OUTLETS`](signal_layer_core::MAX_OUTLETS) outlets.
    Full,
    /// Something with this name is already declared in that registry.
    Duplicate,
}

impl fmt::Display for DeclareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => f.write_str("the registry is full"),
            Self::Duplicate => f.write_str("a slot with this name is already declared"),
        }
    }
}

impl From<TapError> for DeclareError {
    fn from(err: TapError) -> Self {
        match err {
            TapError::Duplicate => Self::Duplicate,
            // `register` fails with nothing but `RegistryFull` otherwise.
            _ => Self::Full,
        }
    }
}

/// Milliseconds since boot: the clock every tap and outlet is stamped with.
pub(crate) fn now() -> Timestamp {
    Timestamp(embassy_time::Instant::now().as_millis())
}

/// A retained tap: holds the latest value, which the cell reads
/// non-destructively.
///
/// `Copy`, so a task can keep one and `setup` can keep another; every copy
/// writes the same slot.
pub struct Tap<T: Clone + Send + 'static> {
    name: &'static str,
    slot: &'static RetainedSlot<T>,
}

impl<T> Tap<T>
where
    T: Serialize + WireType + Clone + Send + Sync + 'static,
{
    pub(crate) fn declare(
        registry: &mut TapRegistry,
        name: &'static str,
    ) -> Result<Self, DeclareError> {
        // Declared once per boot and read by the runtime for the rest of it,
        // so the slot is leaked rather than reference-counted.
        let slot: &'static RetainedSlot<T> = Box::leak(Box::new(RetainedSlot::new()));
        registry.register(name, SlotEntry::retained(slot))?;
        Ok(Self { name, slot })
    }
}

impl<T: Clone + Send + 'static> Tap<T> {
    /// The name a cell resolves this tap by.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Publishes `value`, stamped with the time of the call.
    pub fn update(&self, value: T) {
        self.slot.update(now(), value);
    }

    /// Publishes `value` with an explicit timestamp, for a reading whose time
    /// of acquisition matters more than the time it was processed.
    pub fn update_at(&self, at: Timestamp, value: T) {
        self.slot.update(at, value);
    }

    /// Drops the retained value, so the cell reads nothing rather than a
    /// stale reading. What a source does when its sensor stops answering.
    pub fn clear(&self) {
        self.slot.clear();
    }

    /// The latest published value and its timestamp, if any.
    #[must_use]
    pub fn read(&self) -> Option<(Timestamp, T)> {
        self.slot.read()
    }
}

// Manual rather than derived so the handle is `Copy` for every `T`: a derive
// would bound the impls on `T: Copy`, and the handle is only a `&'static`.
#[expect(
    clippy::expl_impl_clone_on_copy,
    reason = "derive(Clone, Copy) would require T: Copy"
)]
impl<T: Clone + Send + 'static> Clone for Tap<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Clone + Send + 'static> Copy for Tap<T> {}

impl<T: Clone + Send + 'static> fmt::Debug for Tap<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tap")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// An event tap: a small queue the cell drains one event at a time.
///
/// Holds the eight most recent events; emitting into a full queue drops the
/// oldest. For alarms and state changes, where every occurrence matters but a
/// consumer that falls behind should see the latest, not the first.
pub struct EventTap<T: Clone + Send + 'static> {
    name: &'static str,
    slot: &'static EventSlot<T>,
}

impl<T> EventTap<T>
where
    T: Serialize + WireType + Clone + Send + Sync + 'static,
{
    pub(crate) fn declare(
        registry: &mut TapRegistry,
        name: &'static str,
    ) -> Result<Self, DeclareError> {
        let slot: &'static EventSlot<T> = Box::leak(Box::new(EventSlot::new()));
        registry.register(name, SlotEntry::event(slot))?;
        Ok(Self { name, slot })
    }
}

impl<T: Clone + Send + 'static> EventTap<T> {
    /// The name a cell resolves this tap by.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Queues `event` for the cell.
    pub fn emit(&self, event: T) {
        self.slot.emit(event);
    }
}

#[expect(
    clippy::expl_impl_clone_on_copy,
    reason = "derive(Clone, Copy) would require T: Copy"
)]
impl<T: Clone + Send + 'static> Clone for EventTap<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Clone + Send + 'static> Copy for EventTap<T> {}

impl<T: Clone + Send + 'static> fmt::Debug for EventTap<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventTap")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
