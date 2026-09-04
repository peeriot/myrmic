use serde::Serialize;
use serde::de::DeserializeOwned;

use alloc::string::String;

use crate::{Bytes, Result};

/// Turns a raw payload buffer into `Self`.
///
/// Message types get this for free via `#[derive(Message)]`, which delegates to
/// the bound [`Codec`]. The `#[cmd]`/`#[evt]` handler macros call
/// [`from_args`](Decoder::from_args) on the handler's argument type.
pub trait Decoder: Sized {
    /// Reads this invocation's `length`-byte payload from the host (via
    /// [`get_arguments`](crate::get_arguments)) and decodes it with
    /// [`from_bytes`](Self::from_bytes).
    fn from_args(length: usize) -> Result<Self> {
        let mut bytes = alloc::vec![0u8; length];
        let n = crate::get_arguments(&mut bytes).map_err(|_| "failed to read arguments")?;
        bytes.truncate(n);
        Self::from_bytes(bytes)
    }

    /// Decodes `Self` from the raw payload buffer.
    fn from_bytes(bytes: Bytes) -> Result<Self>;
}

/// Turns `Self` into a raw payload buffer.
///
/// Message types get this for free via `#[derive(Message)]`, which delegates to
/// the bound [`Codec`].
pub trait Encoder {
    /// Encodes `self` into a raw payload buffer.
    fn to_bytes(&self) -> Result<Bytes>;
}

/// A wire serialization format.
///
/// Implement this to add a custom codec, then bind it to a message type with
/// `#[derive(Message)]` + `#[codec(YourCodec)]`. The SDK ships [`Json`] and
/// [`Postcard`].
///
/// ```
/// struct MsgPack;
/// impl myrmic_sdk::Codec for MsgPack {
///     fn encode<T: serde::Serialize + ?Sized>(value: &T) -> myrmic_sdk::Result<myrmic_sdk::Bytes> {
///         rmp_serde::to_vec(value).map_err(|_| "encode failed")
///     }
///     fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> myrmic_sdk::Result<T> {
///         rmp_serde::from_slice(bytes).map_err(|_| "decode failed")
///     }
/// }
///
/// use myrmic_sdk::Codec;
/// let bytes = MsgPack::encode(&42u32)?;
/// assert_eq!(MsgPack::decode::<u32>(&bytes)?, 42);
/// # Ok::<(), &'static str>(())
/// ```
pub trait Codec {
    /// Serializes `value` into bytes.
    fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Bytes>;
    /// Deserializes a `T` from `bytes`.
    fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T>;
}

/// JSON codec.
pub struct Json;

impl Codec for Json {
    fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Bytes> {
        serde_json::to_vec(value).map_err(|_| "failed to serialize json")
    }

    fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
        serde_json::from_slice(bytes).map_err(|_| "failed to deserialize json")
    }
}

/// Postcard codec.
pub struct Postcard;

impl Codec for Postcard {
    fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Bytes> {
        postcard::to_allocvec(value).map_err(|_| "failed to serialize postcard")
    }

    fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
        postcard::from_bytes(bytes).map_err(|_| "failed to deserialize postcard")
    }
}

/// Raw, uncodec'd bytes — a handler parameter of type `Bytes` receives the
/// payload verbatim.
impl Decoder for Bytes {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        Ok(bytes)
    }
}

/// Used to send custom payloads, for example, raw image data, etc.
impl Encoder for Bytes {
    fn to_bytes(&self) -> Result<Bytes> {
        Ok(self.clone())
    }
}

/// The absence of a payload. This is the default [`Decoder`] the `#[cmd]` /
/// `#[evt]` macros use for a handler declared with only a `Metadata` parameter:
/// decoding succeeds only when the argument buffer is empty, so sending a
/// payload to such a handler is rejected rather than silently ignored.
pub struct Void;

impl Decoder for Void {
    fn from_args(length: usize) -> Result<Self> {
        if length == 0 {
            Ok(Void)
        } else {
            Err("this handler does not accept a payload")
        }
    }

    fn from_bytes(bytes: Bytes) -> Result<Self> {
        if bytes.is_empty() {
            Ok(Void)
        } else {
            Err("this handler does not accept a payload")
        }
    }
}

impl Encoder for Void {
    fn to_bytes(&self) -> Result<Bytes> {
        Ok(Bytes::new())
    }
}

impl<T: Decoder> Decoder for Option<T> {
    fn from_args(length: usize) -> Result<Self> {
        if length == 0 {
            Ok(None)
        } else {
            T::from_args(length).map(Some)
        }
    }

    fn from_bytes(bytes: Bytes) -> Result<Self> {
        if bytes.is_empty() {
            Ok(None)
        } else {
            T::from_bytes(bytes).map(Some)
        }
    }
}

impl<T: Encoder> Encoder for Option<T> {
    fn to_bytes(&self) -> Result<Bytes> {
        match self {
            Some(value) => T::to_bytes(value),
            None => Ok(Bytes::new()),
        }
    }
}

/// Bare `String`, `char`, `bool`, and float payloads, carried on the wire as
/// JSON.
///
/// A handler parameter — or a `send`/`publish`/callback value — of one of these
/// types is encoded directly, with no wrapper message struct, so a bare scalar
/// travels exactly as an external caller (e.g. the gateway) would naturally send
/// it: `true`, `"hi"`, `1.5`. This matches the [`Json`] default that
/// `#[derive(Message)]` gives struct payloads.
macro_rules! json_scalar {
    ($($ty:ty),* $(,)?) => {$(
        impl Decoder for $ty {
            fn from_bytes(bytes: Bytes) -> Result<Self> {
                <Json as Codec>::decode(&bytes)
            }
        }

        impl Encoder for $ty {
            fn to_bytes(&self) -> Result<Bytes> {
                <Json as Codec>::encode(self)
            }
        }
    )*};
}

// `f32`/`f64` fall here too: `serde_json` already coerces an integer literal
// (`42`) into a float, so a plain decode covers both `42` and `1.5`.
json_scalar!(String, bool, char, f32, f64);

/// Bare integer payloads, carried on the wire as JSON numbers.
///
/// An exact decode is tried first so a full-width integer literal (including
/// 128-bit) round-trips losslessly. Failing that, the payload is read as a
/// [`serde_json::Number`] and accepted when its value is a whole number in range
/// for the target type — so a `42.0` sent for a `u32` still decodes to `42`,
/// while `42.5` or an out-of-range value is rejected.
macro_rules! json_int {
    ($($ty:ty),* $(,)?) => {$(
        impl Decoder for $ty {
            fn from_bytes(bytes: Bytes) -> Result<Self> {
                if let Ok(value) = <Json as Codec>::decode::<$ty>(&bytes) {
                    return Ok(value);
                }
                let number: serde_json::Number = <Json as Codec>::decode(&bytes)?;
                let f = number.as_f64().ok_or("expected a number")?;
                // `f as i128` round-tripping equal proves `f` is a whole number
                // within i128 range; the bounds check then confines it to `$ty`.
                // (`f64::fract` is unavailable under `no_std`.)
                if f as i128 as f64 == f && f >= <$ty>::MIN as f64 && f <= <$ty>::MAX as f64 {
                    Ok(f as $ty)
                } else {
                    Err("number is not a whole value in range for the target type")
                }
            }
        }

        impl Encoder for $ty {
            fn to_bytes(&self) -> Result<Bytes> {
                <Json as Codec>::encode(self)
            }
        }
    )*};
}

json_int!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128,);

#[cfg(test)]
mod tests {
    use super::{Decoder, Encoder};
    use crate::{Bytes, Result};
    use alloc::string::String;
    use alloc::vec::Vec;

    fn dec<T: Decoder>(bytes: &[u8]) -> Result<T> {
        T::from_bytes(Bytes::from(bytes))
    }

    fn enc<T: Encoder>(value: &T) -> Vec<u8> {
        value.to_bytes().unwrap()
    }

    fn enc_str<T: Encoder>(value: &T) -> String {
        String::from_utf8(enc(value)).unwrap()
    }

    #[test]
    fn integer_encodes_as_json_number() {
        assert_eq!(enc_str(&42u32), "42");
    }

    #[test]
    fn json_number_decodes_into_integer() {
        // A bare `42` as the gateway sends it on the wire.
        assert_eq!(dec::<u32>(b"42").unwrap(), 42);
    }

    #[test]
    fn integral_float_decodes_into_integer() {
        assert_eq!(dec::<u32>(b"42.0").unwrap(), 42);
    }

    #[test]
    fn fractional_number_rejected_for_integer() {
        assert!(dec::<u32>(b"42.5").is_err());
    }

    #[test]
    fn out_of_range_number_rejected_for_integer() {
        assert!(dec::<u8>(b"300").is_err());
    }

    #[test]
    fn signed_integer_round_trips() {
        assert_eq!(dec::<i64>(&enc(&-5i64)).unwrap(), -5);
    }

    #[test]
    fn max_u128_round_trips_exactly() {
        let v = u128::MAX;
        assert_eq!(dec::<u128>(&enc(&v)).unwrap(), v);
    }

    #[test]
    fn float_decodes_from_any_json_number() {
        assert_eq!(dec::<f64>(b"42").unwrap(), 42.0);
        assert_eq!(dec::<f32>(b"1.5").unwrap(), 1.5);
    }

    #[test]
    fn float_encodes_as_json_number() {
        assert_eq!(enc_str(&1.5f64), "1.5");
    }

    #[test]
    fn bool_round_trips_as_json() {
        assert_eq!(enc_str(&true), "true");
        assert!(dec::<bool>(b"true").unwrap());
    }

    #[test]
    fn char_round_trips_as_json_string() {
        assert_eq!(enc_str(&'a'), "\"a\"");
        assert_eq!(dec::<char>(b"\"a\"").unwrap(), 'a');
    }

    #[test]
    fn string_round_trips_as_json_string() {
        // A bareword the gateway wraps as a JSON string.
        assert_eq!(enc_str(&String::from("jsontest")), "\"jsontest\"");
        assert_eq!(dec::<String>(b"\"jsontest\"").unwrap(), "jsontest");
    }
}
