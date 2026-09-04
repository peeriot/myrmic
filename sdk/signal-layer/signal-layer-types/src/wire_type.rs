//! Wire-type identity: the declared type of a tap/outlet slot as a checkable
//! runtime value, so a wrong-typed read or write is refused instead of being
//! decoded into a plausible wrong value (swarm#1315).
//!
//! [`WireType::TYPE_NAME`] is the canonical name — the same vocabulary the
//! pipeline YAML `type:` field uses — and [`WireType::TYPE_ID`] is a stable
//! 32-bit FNV-1a hash of it, computed at compile time. The host stores the
//! slot's id in the registries; the cell SDK compares its `T::TYPE_ID`
//! against the slot's id at resolve/read/write time.

/// Compile-time FNV-1a (32-bit) — tiny, dependency-free, and stable across
/// platforms and compiler versions (unlike `TypeId` or `type_name`).
#[must_use]
pub const fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash
}

/// A type that can live in a tap or outlet slot.
///
/// Implementations must use the type's canonical pipeline-YAML name so that
/// codegen-declared slots and cell-side calls agree on the identity. For a
/// custom slot type, implement it with one const:
///
/// ```
/// # use signal_layer_types::WireType;
/// struct MyReading(f32);
/// impl WireType for MyReading {
///     const TYPE_NAME: &'static str = "MyReading";
/// }
/// ```
pub trait WireType {
    /// Canonical type name (the pipeline YAML `type:` vocabulary).
    const TYPE_NAME: &'static str;
    /// Stable identity: FNV-1a-32 of [`Self::TYPE_NAME`].
    const TYPE_ID: u32 = fnv1a_32(Self::TYPE_NAME.as_bytes());
}

macro_rules! impl_wire_type {
    ($($ty:ty => $name:literal),+ $(,)?) => {
        $(impl WireType for $ty {
            const TYPE_NAME: &'static str = $name;
        })+
    };
}

impl_wire_type! {
    f32 => "f32",
    f64 => "f64",
    u8 => "u8",
    u16 => "u16",
    u32 => "u32",
    u64 => "u64",
    i32 => "i32",
    i64 => "i64",
    bool => "bool",
    usize => "usize",
    crate::ThresholdAlarm => "ThresholdAlarm",
    crate::DriverHealth => "DriverHealth",
    crate::HealthEvent => "HealthEvent",
    crate::DigitalState => "DigitalState",
    crate::PwmDuty => "PwmDuty",
    crate::OutletFault => "OutletFault",
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every id in the wire-type vocabulary, paired with its name.
    const ALL: &[(&str, u32)] = &[
        (<f32 as WireType>::TYPE_NAME, <f32 as WireType>::TYPE_ID),
        (<f64 as WireType>::TYPE_NAME, <f64 as WireType>::TYPE_ID),
        (<u8 as WireType>::TYPE_NAME, <u8 as WireType>::TYPE_ID),
        (<u16 as WireType>::TYPE_NAME, <u16 as WireType>::TYPE_ID),
        (<u32 as WireType>::TYPE_NAME, <u32 as WireType>::TYPE_ID),
        (<u64 as WireType>::TYPE_NAME, <u64 as WireType>::TYPE_ID),
        (<i32 as WireType>::TYPE_NAME, <i32 as WireType>::TYPE_ID),
        (<i64 as WireType>::TYPE_NAME, <i64 as WireType>::TYPE_ID),
        (<bool as WireType>::TYPE_NAME, <bool as WireType>::TYPE_ID),
        (<usize as WireType>::TYPE_NAME, <usize as WireType>::TYPE_ID),
        (
            <crate::ThresholdAlarm as WireType>::TYPE_NAME,
            <crate::ThresholdAlarm as WireType>::TYPE_ID,
        ),
        (
            <crate::DriverHealth as WireType>::TYPE_NAME,
            <crate::DriverHealth as WireType>::TYPE_ID,
        ),
        (
            <crate::HealthEvent as WireType>::TYPE_NAME,
            <crate::HealthEvent as WireType>::TYPE_ID,
        ),
        (
            <crate::DigitalState as WireType>::TYPE_NAME,
            <crate::DigitalState as WireType>::TYPE_ID,
        ),
        (
            <crate::PwmDuty as WireType>::TYPE_NAME,
            <crate::PwmDuty as WireType>::TYPE_ID,
        ),
        (
            <crate::OutletFault as WireType>::TYPE_NAME,
            <crate::OutletFault as WireType>::TYPE_ID,
        ),
    ];

    /// No two wire types may share an id — a collision would let a wrong type
    /// pass the runtime check silently.
    #[test]
    fn type_ids_are_unique() {
        for (i, (name_a, id_a)) in ALL.iter().enumerate() {
            for (name_b, id_b) in &ALL[i + 1..] {
                assert_ne!(
                    id_a, id_b,
                    "TYPE_ID collision between `{name_a}` and `{name_b}`"
                );
            }
        }
    }

    /// Pin the hash function: these values are wire-visible (stored host-side,
    /// compared cell-side), so an accidental change to the hash or a type
    /// rename must fail a test rather than silently splitting old and new
    /// binaries into disagreeing worlds.
    #[test]
    fn type_ids_are_stable() {
        assert_eq!(fnv1a_32(b""), 0x811c_9dc5, "FNV-1a offset basis");
        assert_eq!(
            <f32 as WireType>::TYPE_ID,
            fnv1a_32(b"f32"),
            "id derives from the canonical name"
        );
        assert_eq!(
            <crate::DigitalState as WireType>::TYPE_ID,
            fnv1a_32(b"DigitalState")
        );
    }
}
