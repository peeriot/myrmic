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

    /// Decodes a whole key: bytes left over after it are an error, not ignored.
    /// Use this wherever `bytes` should be exactly one key, so a longer key that
    /// merely starts like one can't pass for it.
    fn decode_exact(bytes: &'a [u8]) -> Result<Self, KeyError> {
        let mut decoder = decoder(bytes);
        let value = Self::decode_from(&mut decoder)?;
        let trailing = decoder.remaining();
        if trailing != 0 {
            anyhow::bail!("{trailing} trailing bytes after the key");
        }
        Ok(value)
    }

    fn decode_from(decoder: &mut Decoder<'a>) -> Result<Self, KeyError>;
}

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::StoreKey;

    #[test]
    fn str_round_trips() {
        for s in ["", "a", "sorg", "a*b:c@d", "ünïcödé"] {
            let encoded = s.encode().unwrap();
            assert_eq!(<&str>::decode_exact(&encoded).unwrap(), s);
        }
    }

    #[test]
    fn str_order_is_preserved() {
        let mut names = ["b", "a", "ab", "", "a*", "a:", "aa", "\u{1}"];
        let mut encoded = names.map(|s| s.encode().unwrap());
        names.sort_unstable();
        encoded.sort();
        assert_eq!(
            encoded.map(|e| <&str>::decode_exact(&e).unwrap().to_owned()),
            names
        );
    }

    #[test]
    fn str_containing_nul_is_refused() {
        for s in ["\0", "a\0", "\0a", "a\0*b", "sorg\0*reg\0*p\0:kvADMIN"] {
            assert!(s.encode().is_err(), "{s:?} encoded");
        }
    }

    #[test]
    fn decode_exact_refuses_trailing_bytes() {
        let mut encoded = "k".encode().unwrap();
        encoded.push(b'z');

        assert_eq!(<&str>::decode_from_bytes(&encoded).unwrap(), "k");
        assert!(<&str>::decode_exact(&encoded).is_err());
    }

    #[test]
    fn decode_exact_accepts_a_whole_key() {
        let encoded = 42u64.encode().unwrap();
        assert_eq!(u64::decode_exact(&encoded).unwrap(), 42);

        let bytes: &[u8] = b"id\0with-nul";
        let encoded = bytes.encode().unwrap();
        assert_eq!(<&[u8]>::decode_exact(&encoded).unwrap(), bytes);
    }
}
