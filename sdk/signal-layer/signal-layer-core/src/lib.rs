#![no_std]

extern crate alloc;

use core::any::Any;
use core::cell::RefCell;
use core::marker::PhantomData;

use critical_section::Mutex;
use heapless::Deque as HDeque;
use heapless::Vec as HVec;
use postcard::{take_from_bytes, to_slice};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use signal_layer_types as types;
pub use signal_layer_types::WireType;
// portable_atomic_util::Arc is a drop-in for alloc::sync::Arc that emulates
// atomics via critical-section on targets without hardware atomic support.
pub use portable_atomic_util::Arc;

mod sealed {
    pub trait Sealed {}
}

/// Sealed marker for stream kinds used as `RetainedSlot` type parameters.
/// Reserved/inert — enforcement deferred to the streaming effort.
pub trait StreamKindMarker: sealed::Sealed {}

/// High-rate continuous stream (e.g. raw ADC, vibration). Reserved/inert.
#[derive(Debug)]
pub struct Signal;
/// Derived / periodic measurements (e.g. averaged temperature). Reserved/inert.
#[derive(Debug)]
pub struct Metric;

impl sealed::Sealed for Signal {}
impl sealed::Sealed for Metric {}
impl StreamKindMarker for Signal {}
impl StreamKindMarker for Metric {}

/// Synchronous processing step building block.
///
/// Each step implements this on its `State` type. `step` is synchronous and
/// self-contained: no `.await`, no slot access, no `Arc`. It may keep state
/// across calls, which is what the `State` type is for, but must not reach
/// outside itself. Conditional emission via `Option<Output>`.
///
/// **`step` must not block.** It runs inline on the source's task, which shares
/// a cooperative executor with every other task on the node, including the
/// watchdog feeder. A step that spins or sleeps never yields, so the executor
/// stops turning, the feed is withheld and the device resets. Keep `step` to
/// arithmetic over its input and its own state.
pub trait ProcessingStep {
    type Input;
    type Output;
    fn step(&mut self, input: Self::Input) -> Option<Self::Output>;
}

/// Monotonic timestamp in milliseconds since process start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapError {
    NotFound,
    WrongKind,
    BufferTooSmall,
    Empty,
    RegistryFull,
    /// A command payload failed to decode into the slot's declared type.
    /// Raised on the write path ([`AnyWritable::write_bytes`]) — the "declared
    /// type" half of command validation (OUT-08).
    Decode,
}

/// Holds the latest `(Timestamp, T)`. Updated by the native side; read by WASM.
///
/// `K` is a [`StreamKindMarker`] encoding stream kind at the type level;
/// erased at the `Box<dyn AnyRetained>` boundary.
pub struct RetainedSlot<T: Clone + Send + 'static, K: StreamKindMarker = Metric> {
    inner: Mutex<RefCell<Option<(Timestamp, T)>>>,
    _kind: PhantomData<K>,
}

impl<T: Clone + Send + 'static, K: StreamKindMarker> RetainedSlot<T, K> {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(RefCell::new(None)),
            _kind: PhantomData,
        }
    }

    pub fn update(&self, ts: Timestamp, val: T) {
        critical_section::with(|cs| {
            *self.inner.borrow(cs).borrow_mut() = Some((ts, val));
        });
    }

    /// Drop the retained value, returning the slot to its pre-first-update state.
    ///
    /// Subsequent [`read`](Self::read) calls return `None` (and `read_bytes`
    /// returns [`TapError::Empty`]) until the next [`update`](Self::update).
    /// Used by source tasks to invalidate stale readings when a driver leaves
    /// the healthy state, so consumers never observe a value produced before a
    /// fault as if it were current.
    pub fn clear(&self) {
        critical_section::with(|cs| {
            *self.inner.borrow(cs).borrow_mut() = None;
        });
    }

    pub fn read(&self) -> Option<(Timestamp, T)> {
        critical_section::with(|cs| self.inner.borrow(cs).borrow().clone())
    }
}

impl<T: Clone + Send + 'static, K: StreamKindMarker> Default for RetainedSlot<T, K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + Send + core::fmt::Debug + 'static, K: StreamKindMarker> core::fmt::Debug
    for RetainedSlot<T, K>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let val = critical_section::with(|cs| self.inner.borrow(cs).borrow().clone());
        f.debug_struct("RetainedSlot")
            .field("inner", &val)
            .field("_kind", &self._kind)
            .finish()
    }
}

/// Bounded ring buffer; drops oldest on overflow.
#[derive(Debug)]
pub struct BatchSlot<T: Clone, const N: usize> {
    buf: HDeque<T, N>,
    dropped: u32,
}

impl<T: Clone, const N: usize> BatchSlot<T, N> {
    pub const fn new() -> Self {
        Self {
            buf: HDeque::new(),
            dropped: 0,
        }
    }

    pub fn push(&mut self, val: T) {
        if self.buf.is_full() {
            self.buf.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        drop(self.buf.push_back(val));
    }

    #[expect(clippy::missing_panics_doc, reason = "We know it cannot panic")]
    pub fn drain(&mut self, out: &mut [T]) -> usize {
        let count = out.len().min(self.buf.len());
        for dst in out.iter_mut().take(count) {
            #[expect(clippy::unwrap_used, reason = "count <= len")]
            {
                *dst = self.buf.pop_front().unwrap();
            }
        }
        count
    }

    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

impl<T: Clone, const N: usize> Default for BatchSlot<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

const EVENT_CAPACITY: usize = 8;

/// Small bounded queue; overwrites oldest on overflow.
pub struct EventSlot<T: Clone + Send + 'static> {
    inner: Mutex<RefCell<HVec<T, EVENT_CAPACITY>>>,
}

impl<T: Clone + Send + 'static> EventSlot<T> {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(RefCell::new(HVec::new())),
        }
    }

    pub fn emit(&self, val: T) {
        critical_section::with(|cs| {
            let mut q = self.inner.borrow(cs).borrow_mut();
            if q.len() == EVENT_CAPACITY {
                q.remove(0);
            }
            drop(q.push(val));
        });
    }

    pub fn take(&self) -> Option<T> {
        critical_section::with(|cs| {
            let mut q = self.inner.borrow(cs).borrow_mut();
            if q.is_empty() {
                None
            } else {
                Some(q.remove(0))
            }
        })
    }
}

impl<T: Clone + Send + 'static> Default for EventSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + Send + core::fmt::Debug + 'static> core::fmt::Debug for EventSlot<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let val = critical_section::with(|cs| self.inner.borrow(cs).borrow().clone());
        f.debug_struct("EventSlot").field("inner", &val).finish()
    }
}

/// Serialise the current retained value into `buf` via postcard.
pub trait AnyRetained: Send + Sync + Any {
    fn read_bytes(&self, ts_out: &mut u64, buf: &mut [u8]) -> Result<usize, TapError>;
    /// The slot's declared wire type ([`WireType::TYPE_ID`]), so a cell's
    /// typed read can be checked against it (swarm#1315).
    fn wire_type_id(&self) -> u32;
}

/// Blanket impl: any `T: Serialize + WireType + Clone + Send + Sync` works
/// without editing this crate. `f32` encodes to 4 little-endian bytes —
/// identical to the old `to_le_bytes` path (postcard uses IEEE 754 LE for f32).
impl<T, K> AnyRetained for RetainedSlot<T, K>
where
    T: Serialize + WireType + Clone + Send + Sync + 'static,
    K: StreamKindMarker + Send + Sync + 'static,
{
    fn read_bytes(&self, ts_out: &mut u64, buf: &mut [u8]) -> Result<usize, TapError> {
        let (ts, val) = self.read().ok_or(TapError::Empty)?;
        *ts_out = ts.0;
        to_slice(&val, buf)
            .map(|used| used.len())
            .map_err(|_err| TapError::BufferTooSmall)
    }

    fn wire_type_id(&self) -> u32 {
        T::TYPE_ID
    }
}

// Delegation so Arc<T: AnyRetained> can be boxed into Box<dyn AnyRetained>.
// portable_atomic_util::Arc lacks CoerceUnsized so we store Box<dyn> instead.
impl<T: AnyRetained + 'static> AnyRetained for Arc<T> {
    fn read_bytes(&self, ts_out: &mut u64, buf: &mut [u8]) -> Result<usize, TapError> {
        (**self).read_bytes(ts_out, buf)
    }

    fn wire_type_id(&self) -> u32 {
        (**self).wire_type_id()
    }
}

// Delegation so a `&'static RetainedSlot` (codegen-emitted statics) can be boxed
// into `Box<dyn AnyRetained>` and registered without an owning allocation.
impl<T: AnyRetained + 'static> AnyRetained for &'static T {
    fn read_bytes(&self, ts_out: &mut u64, buf: &mut [u8]) -> Result<usize, TapError> {
        (**self).read_bytes(ts_out, buf)
    }

    fn wire_type_id(&self) -> u32 {
        (**self).wire_type_id()
    }
}

/// Serialise and remove the next pending event from the queue via postcard.
pub trait AnyEvent: Send + Sync + Any {
    fn take_bytes(&self, buf: &mut [u8]) -> Result<usize, TapError>;
    /// The slot's declared wire type ([`WireType::TYPE_ID`]).
    fn wire_type_id(&self) -> u32;
}

/// Blanket impl: any `T: Serialize + Clone + Send + Sync` works without editing
/// this crate.
impl<T> AnyEvent for EventSlot<T>
where
    T: Serialize + WireType + Clone + Send + Sync + 'static,
{
    fn take_bytes(&self, buf: &mut [u8]) -> Result<usize, TapError> {
        let val = self.take().ok_or(TapError::Empty)?;
        to_slice(&val, buf)
            .map(|used| used.len())
            .map_err(|_err| TapError::BufferTooSmall)
    }

    fn wire_type_id(&self) -> u32 {
        T::TYPE_ID
    }
}

impl<T: AnyEvent + 'static> AnyEvent for Arc<T> {
    fn take_bytes(&self, buf: &mut [u8]) -> Result<usize, TapError> {
        (**self).take_bytes(buf)
    }

    fn wire_type_id(&self) -> u32 {
        (**self).wire_type_id()
    }
}

impl<T: AnyEvent + 'static> AnyEvent for &'static T {
    fn take_bytes(&self, buf: &mut [u8]) -> Result<usize, TapError> {
        (**self).take_bytes(buf)
    }

    fn wire_type_id(&self) -> u32 {
        (**self).wire_type_id()
    }
}

/// Drain serialised frames out of a batch slot into a flat byte buffer.
pub trait AnyBatch: Send + Sync + Any {
    /// Drain available frames into `buf` (postcard-encoded), returning bytes written.
    fn drain_bytes(&self, buf: &mut [u8]) -> Result<usize, TapError>;
    /// The slot's declared wire type ([`WireType::TYPE_ID`]).
    fn wire_type_id(&self) -> u32;
}

impl<T: AnyBatch + 'static> AnyBatch for Arc<T> {
    fn drain_bytes(&self, buf: &mut [u8]) -> Result<usize, TapError> {
        (**self).drain_bytes(buf)
    }

    fn wire_type_id(&self) -> u32 {
        (**self).wire_type_id()
    }
}

impl<T: AnyBatch + 'static> AnyBatch for &'static T {
    fn drain_bytes(&self, buf: &mut [u8]) -> Result<usize, TapError> {
        (**self).drain_bytes(buf)
    }

    fn wire_type_id(&self) -> u32 {
        (**self).wire_type_id()
    }
}

/// Write-side mirror of [`AnyRetained`]: deserialise `bytes` (postcard) into the
/// slot as the latest command value. This is how a WASM cell drives an
/// [`OutletRegistry`] entry through the host `outlet` module.
///
/// A successful decode is the "declared type" half of command validation
/// (OUT-08): bytes that do not decode into the slot's `T` are rejected with
/// [`TapError::Decode`]. Output-mode and allowed-range enforcement live in the
/// backing driver, not here.
pub trait AnyWritable: Send + Sync + Any {
    fn write_bytes(&self, ts: Timestamp, bytes: &[u8]) -> Result<(), TapError>;
    /// The slot's declared wire type ([`WireType::TYPE_ID`]).
    fn wire_type_id(&self) -> u32;
}

/// Blanket impl: any `T: DeserializeOwned + WireType + Clone + Send + Sync` is
/// writable without editing this crate. Last write wins — there is no
/// arbitration or merge, which is how single-writer ownership (F1) is embodied
/// at runtime; the static single-writer guarantee is enforced at
/// manifest/codegen time.
///
/// The decode is strict (swarm#1315): trailing bytes after the decoded value
/// mean the payload was produced for a different type whose prefix happened to
/// parse, and are rejected as [`TapError::Decode`] rather than silently
/// discarded.
impl<T, K> AnyWritable for RetainedSlot<T, K>
where
    T: DeserializeOwned + WireType + Clone + Send + Sync + 'static,
    K: StreamKindMarker + Send + Sync + 'static,
{
    fn write_bytes(&self, ts: Timestamp, bytes: &[u8]) -> Result<(), TapError> {
        let (val, rest): (T, _) = take_from_bytes(bytes).map_err(|_err| TapError::Decode)?;
        if !rest.is_empty() {
            return Err(TapError::Decode);
        }
        self.update(ts, val);
        Ok(())
    }

    fn wire_type_id(&self) -> u32 {
        T::TYPE_ID
    }
}

// Delegation so Arc<T: AnyWritable> can be boxed into Box<dyn AnyWritable>.
impl<T: AnyWritable + 'static> AnyWritable for Arc<T> {
    fn write_bytes(&self, ts: Timestamp, bytes: &[u8]) -> Result<(), TapError> {
        (**self).write_bytes(ts, bytes)
    }

    fn wire_type_id(&self) -> u32 {
        (**self).wire_type_id()
    }
}

// Delegation so a `&'static RetainedSlot` (codegen-emitted static) can be boxed
// into `Box<dyn AnyWritable>` and registered without an owning allocation.
impl<T: AnyWritable + 'static> AnyWritable for &'static T {
    fn write_bytes(&self, ts: Timestamp, bytes: &[u8]) -> Result<(), TapError> {
        (**self).write_bytes(ts, bytes)
    }

    fn wire_type_id(&self) -> u32 {
        (**self).wire_type_id()
    }
}

pub const MAX_TAPS: usize = 16;

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapKind {
    Retained = 0,
    Event = 1,
    Batch = 2,
}

pub enum SlotEntry {
    Retained(alloc::boxed::Box<dyn AnyRetained>),
    Event(alloc::boxed::Box<dyn AnyEvent>),
    Batch(alloc::boxed::Box<dyn AnyBatch>),
}

impl core::fmt::Debug for SlotEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            SlotEntry::Retained(_) => "Retained(..)",
            SlotEntry::Event(_) => "Event(..)",
            SlotEntry::Batch(_) => "Batch(..)",
        })
    }
}

impl SlotEntry {
    pub fn kind(&self) -> TapKind {
        match self {
            SlotEntry::Retained(_) => TapKind::Retained,
            SlotEntry::Event(_) => TapKind::Event,
            SlotEntry::Batch(_) => TapKind::Batch,
        }
    }

    /// The slot's declared wire type ([`WireType::TYPE_ID`]).
    pub fn wire_type_id(&self) -> u32 {
        match self {
            SlotEntry::Retained(r) => r.wire_type_id(),
            SlotEntry::Event(e) => e.wire_type_id(),
            SlotEntry::Batch(b) => b.wire_type_id(),
        }
    }

    /// Wrap a `&'static` retained slot (codegen-emitted static) as a registry entry.
    pub fn retained<R: AnyRetained + 'static>(slot: &'static R) -> Self {
        SlotEntry::Retained(alloc::boxed::Box::new(slot))
    }

    /// Wrap a `&'static` event slot (codegen-emitted static) as a registry entry.
    pub fn event<E: AnyEvent + 'static>(slot: &'static E) -> Self {
        SlotEntry::Event(alloc::boxed::Box::new(slot))
    }

    /// Wrap a `&'static` batch slot (codegen-emitted static) as a registry entry.
    pub fn batch<B: AnyBatch + 'static>(slot: &'static B) -> Self {
        SlotEntry::Batch(alloc::boxed::Box::new(slot))
    }
}

/// Named tap registry with bounded heapless storage.
///
/// Taps are registered by name and resolved to integer handles at init time.
/// After resolve, all access is O(1) by handle index.
#[derive(Debug)]
pub struct TapRegistry {
    slots: HVec<(&'static str, SlotEntry), MAX_TAPS>,
}

impl TapRegistry {
    pub fn new() -> Self {
        Self { slots: HVec::new() }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "MAX_TAPS is 16, so len() never exceeds u32::MAX."
    )]
    pub fn register(&mut self, name: &'static str, slot: SlotEntry) -> Result<u32, TapError> {
        let handle = self.slots.len() as u32;
        self.slots
            .push((name, slot))
            .map_err(|_err| TapError::RegistryFull)?;
        Ok(handle)
    }

    /// Linear scan — intended for init time only.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "MAX_TAPS is 16, so the enumeration index never exceeds u32::MAX."
    )]
    pub fn resolve(&self, name: &str) -> Option<u32> {
        self.slots
            .iter()
            .enumerate()
            .find(|(_, (n, _))| *n == name)
            .map(|(i, _)| i as u32)
    }

    pub fn get(&self, handle: u32) -> Option<&SlotEntry> {
        self.slots.get(handle as usize).map(|(_, s)| s)
    }

    pub fn name_at(&self, handle: u32) -> Option<&'static str> {
        self.slots.get(handle as usize).map(|(n, _)| *n)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

impl Default for TapRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub const MAX_OUTLETS: usize = 8;

/// A registered outlet slot. Retained-only for v1 (actuator commands are
/// last-value-wins); the enum leaves room for an event ("pulse") variant later
/// without breaking the `outlet` host ABI.
pub enum OutletEntry {
    Retained(alloc::boxed::Box<dyn AnyWritable>),
}

impl core::fmt::Debug for OutletEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            OutletEntry::Retained(_) => "Retained(..)",
        })
    }
}

impl OutletEntry {
    pub fn kind(&self) -> TapKind {
        match self {
            OutletEntry::Retained(_) => TapKind::Retained,
        }
    }

    /// The outlet's declared command type ([`WireType::TYPE_ID`]).
    pub fn wire_type_id(&self) -> u32 {
        match self {
            OutletEntry::Retained(w) => w.wire_type_id(),
        }
    }

    /// Wrap a `&'static` retained slot (codegen-emitted static) as a writable outlet entry.
    pub fn retained<W: AnyWritable + 'static>(slot: &'static W) -> Self {
        OutletEntry::Retained(alloc::boxed::Box::new(slot))
    }

    /// Deserialise `bytes` into the outlet's slot as the latest command value.
    pub fn write_bytes(&self, ts: Timestamp, bytes: &[u8]) -> Result<(), TapError> {
        match self {
            OutletEntry::Retained(w) => w.write_bytes(ts, bytes),
        }
    }
}

/// Named outlet registry with bounded heapless storage — the write-side mirror
/// of [`TapRegistry`].
///
/// Outlets are registered by name and resolved to integer handles at init time.
/// A separate registry (and namespace) from taps: a cell resolves outlets via
/// the `outlet` host module and can only ever write them, never a sensor tap.
/// After resolve, all access is O(1) by handle index.
#[derive(Debug)]
pub struct OutletRegistry {
    slots: HVec<(&'static str, OutletEntry), MAX_OUTLETS>,
}

impl OutletRegistry {
    pub fn new() -> Self {
        Self { slots: HVec::new() }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "MAX_OUTLETS is 8, so len() never exceeds u32::MAX."
    )]
    pub fn register(&mut self, name: &'static str, slot: OutletEntry) -> Result<u32, TapError> {
        let handle = self.slots.len() as u32;
        self.slots
            .push((name, slot))
            .map_err(|_err| TapError::RegistryFull)?;
        Ok(handle)
    }

    /// Linear scan — intended for init time only.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "MAX_OUTLETS is 8, so the enumeration index never exceeds u32::MAX."
    )]
    pub fn resolve(&self, name: &str) -> Option<u32> {
        self.slots
            .iter()
            .enumerate()
            .find(|(_, (n, _))| *n == name)
            .map(|(i, _)| i as u32)
    }

    pub fn get(&self, handle: u32) -> Option<&OutletEntry> {
        self.slots.get(handle as usize).map(|(_, s)| s)
    }

    pub fn name_at(&self, handle: u32) -> Option<&'static str> {
        self.slots.get(handle as usize).map(|(n, _)| *n)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

impl Default for OutletRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use postcard::from_bytes;
    use signal_layer_types::{DriverHealth, HealthEvent, ThresholdAlarm};

    // f32 wire-format compatibility: postcard encodes f32 as 4 LE bytes,
    // identical to the old f32::to_le_bytes() path.
    #[test]
    fn f32_retained_round_trip() {
        let slot: RetainedSlot<f32> = RetainedSlot::new();
        slot.update(Timestamp(42), 2.5_f32);

        let mut ts = 0u64;
        let mut buf = [0u8; 16];
        let n = slot.read_bytes(&mut ts, &mut buf).unwrap();

        assert_eq!(ts, 42);
        assert_eq!(n, 4);
        assert_eq!(&buf[..4], &2.5_f32.to_le_bytes());

        let decoded: f32 = from_bytes(&buf[..n]).unwrap();
        assert_eq!(decoded, 2.5_f32);
    }

    #[test]
    fn retained_clear_drops_value() {
        let slot: RetainedSlot<f32> = RetainedSlot::new();
        slot.update(Timestamp(42), 2.5_f32);
        assert_eq!(slot.read(), Some((Timestamp(42), 2.5_f32)));

        slot.clear();
        assert_eq!(slot.read(), None);

        // read_bytes mirrors the pre-first-update state: Empty.
        let mut ts = 0u64;
        let mut buf = [0u8; 16];
        assert!(matches!(
            slot.read_bytes(&mut ts, &mut buf),
            Err(TapError::Empty)
        ));

        // A later update repopulates the slot.
        slot.update(Timestamp(99), 7.0_f32);
        assert_eq!(slot.read(), Some((Timestamp(99), 7.0_f32)));
    }

    #[test]
    fn threshold_alarm_round_trip() {
        let slot: EventSlot<ThresholdAlarm> = EventSlot::new();
        slot.emit(ThresholdAlarm {
            value: 25.5,
            threshold: 20.0,
        });

        let mut buf = [0u8; 32];
        let n = slot.take_bytes(&mut buf).unwrap();

        let decoded: ThresholdAlarm = from_bytes(&buf[..n]).unwrap();
        assert_eq!(decoded.value, 25.5);
        assert_eq!(decoded.threshold, 20.0);
    }

    #[test]
    fn driver_health_round_trip() {
        let slot: EventSlot<HealthEvent> = EventSlot::new();
        slot.emit(HealthEvent {
            source: 1,
            state: DriverHealth::Degraded,
        });

        let mut buf = [0u8; 16];
        let n = slot.take_bytes(&mut buf).unwrap();

        let decoded: HealthEvent = from_bytes(&buf[..n]).unwrap();
        assert_eq!(decoded.source, 1);
        assert_eq!(decoded.state, DriverHealth::Degraded);
    }

    /// The ticket's concrete case (swarm#1315): `PwmDuty { duty: 0.5 }`
    /// encodes to 4 bytes whose first byte parses as `DigitalState`'s bool —
    /// before the strict decode this was accepted and switched the relay off.
    #[test]
    fn wrong_typed_outlet_write_is_rejected() {
        use signal_layer_types::{DigitalState, PwmDuty};

        let slot: RetainedSlot<DigitalState> = RetainedSlot::new();
        let mut pwm_bytes = [0u8; 8];
        let pwm_bytes = postcard::to_slice(&PwmDuty { duty: 0.5 }, &mut pwm_bytes).unwrap();

        let result = AnyWritable::write_bytes(&slot, Timestamp(1), pwm_bytes);
        assert!(
            matches!(result, Err(TapError::Decode)),
            "PwmDuty payload into a DigitalState slot must be rejected, got {result:?}"
        );
        assert!(slot.read().is_none(), "the slot must remain unwritten");
    }

    /// Strict decode: a correct value followed by trailing bytes is a payload
    /// produced for a different type — rejected, not silently truncated.
    #[test]
    fn trailing_bytes_on_write_are_rejected() {
        use signal_layer_types::DigitalState;

        let slot: RetainedSlot<DigitalState> = RetainedSlot::new();
        // Valid `DigitalState { on: true }` byte plus one stray trailing byte.
        let result = AnyWritable::write_bytes(&slot, Timestamp(1), &[1, 0xAA]);
        assert!(matches!(result, Err(TapError::Decode)));

        // The exact payload still works.
        AnyWritable::write_bytes(&slot, Timestamp(2), &[1]).unwrap();
        assert_eq!(slot.read().unwrap().1, DigitalState { on: true });
    }

    /// Registry entries expose the declared wire type for the host functions.
    #[test]
    fn entries_expose_wire_type_id() {
        use signal_layer_types::{DigitalState, WireType};

        static RETAINED: RetainedSlot<f32> = RetainedSlot::new();
        static EVENT: EventSlot<ThresholdAlarm> = EventSlot::new();
        static OUTLET: RetainedSlot<DigitalState> = RetainedSlot::new();

        assert_eq!(
            SlotEntry::retained(&RETAINED).wire_type_id(),
            <f32 as WireType>::TYPE_ID
        );
        assert_eq!(
            SlotEntry::event(&EVENT).wire_type_id(),
            <ThresholdAlarm as WireType>::TYPE_ID
        );
        assert_eq!(
            OutletEntry::retained(&OUTLET).wire_type_id(),
            <DigitalState as WireType>::TYPE_ID
        );
    }

    // Adding a new payload type requires zero edits to signal-layer-core —
    // only a one-const `WireType` impl naming the type for the runtime check.
    #[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct CustomPayload {
        x: u32,
        label: u8,
    }

    impl signal_layer_types::WireType for CustomPayload {
        const TYPE_NAME: &'static str = "CustomPayload";
    }

    #[test]
    fn custom_payload_no_core_edit_needed() {
        let slot: RetainedSlot<CustomPayload> = RetainedSlot::new();
        slot.update(Timestamp(0), CustomPayload { x: 99, label: 7 });

        let mut ts = 0u64;
        let mut buf = [0u8; 16];
        let n = slot.read_bytes(&mut ts, &mut buf).unwrap();

        let decoded: CustomPayload = from_bytes(&buf[..n]).unwrap();
        assert_eq!(decoded, CustomPayload { x: 99, label: 7 });
    }

    #[test]
    fn empty_slot_returns_empty_error() {
        let slot: RetainedSlot<f32> = RetainedSlot::new();
        let mut ts = 0u64;
        let mut buf = [0u8; 8];
        assert_eq!(slot.read_bytes(&mut ts, &mut buf), Err(TapError::Empty));
    }

    #[test]
    fn buffer_too_small_returns_error() {
        let slot: RetainedSlot<f32> = RetainedSlot::new();
        slot.update(Timestamp(0), 1.0_f32);
        let mut ts = 0u64;
        let mut buf = [0u8; 1]; // too small for 4-byte f32
        assert_eq!(
            slot.read_bytes(&mut ts, &mut buf),
            Err(TapError::BufferTooSmall)
        );
    }

    // ---- Outlet write path (mirror of the read path above) ----

    #[test]
    fn outlet_write_bytes_round_trip() {
        use signal_layer_types::DigitalState;

        // A cell writes a postcard-encoded command; the driver reads it back.
        let slot: RetainedSlot<DigitalState> = RetainedSlot::new();
        let mut buf = [0u8; 8];
        let bytes = postcard::to_slice(&DigitalState { on: true }, &mut buf).unwrap();

        AnyWritable::write_bytes(&slot, Timestamp(7), bytes).unwrap();

        assert_eq!(slot.read(), Some((Timestamp(7), DigitalState { on: true })));
    }

    #[test]
    fn outlet_write_last_write_wins() {
        // No arbitration: the most recent command is the one a driver observes (F1).
        let slot: RetainedSlot<f32> = RetainedSlot::new();
        let mut buf = [0u8; 8];

        let first = postcard::to_slice(&0.25_f32, &mut buf).unwrap();
        AnyWritable::write_bytes(&slot, Timestamp(1), first).unwrap();
        let n = first.len();
        let second = postcard::to_slice(&0.75_f32, &mut buf[n..]).unwrap();
        AnyWritable::write_bytes(&slot, Timestamp(2), second).unwrap();

        assert_eq!(slot.read(), Some((Timestamp(2), 0.75_f32)));
    }

    #[test]
    fn outlet_write_rejects_malformed_payload() {
        // A truncated f32 payload cannot decode → Decode error, slot untouched.
        let slot: RetainedSlot<f32> = RetainedSlot::new();
        assert_eq!(
            AnyWritable::write_bytes(&slot, Timestamp(0), &[0x00]),
            Err(TapError::Decode)
        );
        assert_eq!(slot.read(), None);
    }

    #[test]
    fn outlet_registry_resolve_and_write() {
        use signal_layer_types::PwmDuty;

        static FAN: RetainedSlot<PwmDuty> = RetainedSlot::new();
        let mut registry = OutletRegistry::new();
        let handle = registry
            .register("fan.duty", OutletEntry::retained(&FAN))
            .unwrap();

        assert_eq!(registry.resolve("fan.duty"), Some(handle));
        assert_eq!(registry.resolve("missing"), None);
        assert_eq!(registry.name_at(handle), Some("fan.duty"));
        assert_eq!(
            registry.get(handle).map(OutletEntry::kind),
            Some(TapKind::Retained)
        );

        let mut buf = [0u8; 8];
        let bytes = postcard::to_slice(&PwmDuty { duty: 0.5 }, &mut buf).unwrap();
        registry
            .get(handle)
            .unwrap()
            .write_bytes(Timestamp(3), bytes)
            .unwrap();

        assert_eq!(FAN.read(), Some((Timestamp(3), PwmDuty { duty: 0.5 })));
    }

    #[test]
    fn outlet_registry_full_returns_error() {
        static SLOTS: [RetainedSlot<f32>; MAX_OUTLETS] =
            [const { RetainedSlot::new() }; MAX_OUTLETS];
        // Leak stable &'static str names for the registry entries.
        const NAMES: [&str; MAX_OUTLETS] = ["o0", "o1", "o2", "o3", "o4", "o5", "o6", "o7"];

        let mut registry = OutletRegistry::new();
        for i in 0..MAX_OUTLETS {
            registry
                .register(NAMES[i], OutletEntry::retained(&SLOTS[i]))
                .unwrap();
        }
        assert_eq!(registry.len(), MAX_OUTLETS);

        static OVERFLOW: RetainedSlot<f32> = RetainedSlot::new();
        assert_eq!(
            registry.register("overflow", OutletEntry::retained(&OVERFLOW)),
            Err(TapError::RegistryFull)
        );
    }
}
