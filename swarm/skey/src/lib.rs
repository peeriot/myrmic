//! This crate exists because of two reasons:
//!
//! 1) Introduce a single abstract type that represents a store key.
//!    This lets us build simple abstractions.
//!    We also added a derive macro to aid impl of this trait.
//!
//! 2) Support nested types.
//!    This crates solves that by using the aforementioned abstract type to defer the de/serialisation calls.

mod impls;

mod decode;
mod encode;

#[cfg(feature = "derive")]
pub use skey_macros::StoreKey;

pub use decode::read::SliceReader;

pub type Encoder<'a> = encode::Serializer<&'a mut Vec<u8>>;
pub type Decoder<'a> = decode::Deserializer<SliceReader<'a>>;
pub type KeyError = anyhow::Error;

pub fn encode<F>(func: F) -> Result<Vec<u8>, KeyError>
where
    F: for<'a, 'b> FnOnce(&'a mut Encoder<'b>) -> Result<(), KeyError>,
{
    let mut writer = vec![];
    let mut encoder = Encoder::new(&mut writer);
    func(&mut encoder)?;
    Ok(writer)
}

pub fn decoder(bytes: &[u8]) -> Decoder<'_> {
    let reader = SliceReader::new(bytes);
    Decoder::new(reader)
}

/// Half-open range `[lower, upper)` covering every key that begins with `prefix`.
///
/// `lower` is `prefix` itself; `upper` is the prefix successor (the smallest key strictly greater
/// than every continuation of `prefix`)
///
/// `upper` is `None` when `prefix` is empty or all `\xFF`, i.e. the range is unbounded above.
pub fn prefix_to_range(prefix: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    let mut upper = prefix.to_vec();

    while let Some(last) = upper.last_mut() {
        if *last < u8::MAX {
            *last += 1;
            return (prefix.to_vec(), Some(upper));
        }
        upper.pop();
    }

    (prefix.to_vec(), None)
}

pub fn expect<'a, T: StoreKey<'a> + PartialEq>(
    expected: &T,
    decoder: &mut Decoder<'a>,
) -> Result<(), KeyError> {
    let value: T = StoreKey::decode_from(decoder)?;
    if &value != expected {
        anyhow::bail!("literal segment mismatch while decoding key")
    }
    Ok(())
}

/// Represents a type that can be represented as a lexigraphically ordered key.
/// If you're manually implementing this type care needs to be taken in order to make sure that regardless of the state space,
/// there is an exact ordering over the type. (this could mean padding bytes to a fixed width, etc)
///
/// A derive macro is also provided.
pub trait StoreKey<'a>: Sized {
    fn range(&self) -> Result<(Vec<u8>, Vec<u8>), KeyError> {
        let (lower, upper) = prefix_to_range(&self.encode()?);
        // Structured keys are never empty or all-`0xFF`, so a successor always exists.
        let upper = upper.ok_or_else(|| anyhow::anyhow!("key prefix has no range upper bound"))?;
        Ok((lower, upper))
    }

    fn encode(&self) -> Result<Vec<u8>, KeyError> {
        let mut writer = vec![];
        let mut encoder = Encoder::new(&mut writer);
        self.encode_into(&mut encoder)?;
        Ok(writer)
    }

    fn encode_into(&self, encoder: &mut Encoder<'_>) -> Result<(), KeyError>;

    fn decode_from_bytes(bytes: &'a [u8]) -> Result<Self, KeyError> {
        let reader = SliceReader::new(bytes);
        let mut decoder = Decoder::new(reader);
        Self::decode_from(&mut decoder)
    }

    fn decode_from(decoder: &mut Decoder<'a>) -> Result<Self, KeyError>;
}
