/// This trait is used in conjunction with serde's `with` attr.
/// Simply implement it on a type, and then call `serde(with = "crate::bypass")`.
///
/// Naturally, this has some restrictions.
/// Either this trait or the type need to be defined in the same crate.
/// Otherwise we can't impl it.
///
/// This is just used to implement a serialisation strategy for some rdf datatypes.
/// (You can find it in `crate::semantic::term`)
pub trait Bypass: Sized {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer;

    fn deserialize<'de, D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>;
}

pub fn serialize<T: Bypass, S>(value: &T, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    <T as Bypass>::serialize(value, s)
}

pub fn deserialize<'de, T: Bypass, D>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <T as Bypass>::deserialize(d)
}
