use anyhow::Context as _;
use generic_array::GenericArray;

use serde::Serializer;

macro_rules! defer_serde {
    (
        $(
            $ty:ty
        ),* $(,)?
    ) => {
        const _: () = {
            $(
                #[cfg(feature = "serde")]
                impl<'a> crate::StoreKey<'a> for $ty {
                    fn encode_into(&self, encoder: &mut crate::Encoder<'_>) -> Result<(), crate::KeyError> {
                        <Self as serde::Serialize>::serialize(self, encoder).context(concat!("Unable to encode ", stringify!($ty)))
                    }

                    fn decode_from(decoder: &mut crate::Decoder<'a>) -> Result<Self, crate::KeyError> {
                        <Self as serde::Deserialize>::deserialize(decoder).context(concat!("Unable to decode ", stringify!($ty)))
                    }
                }
            )*
        };
    };
}

defer_serde! {
    u8, u16, u32, u64
}

#[cfg(feature = "serde")]
impl<'a, const N: usize> crate::StoreKey<'a> for [u8; N]
where
    generic_array::typenum::Const<N>: generic_array::IntoArrayLength,
{
    fn encode_into(&self, encoder: &mut crate::Encoder<'_>) -> Result<(), crate::KeyError> {
        let array = GenericArray::from_array(*self);
        <_ as serde::Serialize>::serialize(&array, encoder).context("Unable to encode byte array")
    }

    fn decode_from(decoder: &mut crate::Decoder<'a>) -> Result<Self, crate::KeyError> {
        let array: GenericArray<u8, _> = <_ as serde::Deserialize>::deserialize(decoder)
            .context("Unable to decode byte array")?;
        Ok(array.into_array())
    }
}

#[cfg(feature = "serde")]
impl<'a> crate::StoreKey<'a> for &'a str {
    fn encode_into(&self, encoder: &mut crate::Encoder<'_>) -> Result<(), crate::KeyError> {
        <Self as serde::Serialize>::serialize(self, encoder).context("Unable to encode str")
    }

    fn decode_from(decoder: &mut crate::Decoder<'a>) -> Result<Self, crate::KeyError> {
        <Self as serde::Deserialize>::deserialize(decoder).context("Unable to decode str")
    }
}

#[cfg(feature = "serde")]
impl<'a> crate::StoreKey<'a> for &'a [u8] {
    fn encode_into(&self, encoder: &mut crate::Encoder<'_>) -> Result<(), crate::KeyError> {
        encoder
            .serialize_bytes(self)
            .context("Unable to encode byte slice")
    }

    fn decode_from(decoder: &mut crate::Decoder<'a>) -> Result<Self, crate::KeyError> {
        <Self as serde::Deserialize>::deserialize(decoder).context("Unable to decode byte slice")
    }
}

#[cfg(feature = "serde")]
impl<'a> crate::StoreKey<'a> for uhlc::Timestamp {
    fn encode_into(&self, encoder: &mut crate::Encoder<'_>) -> Result<(), crate::KeyError> {
        <Self as serde::Serialize>::serialize(self, encoder).context("Unable to encode timestamp")
    }

    fn decode_from(decoder: &mut crate::Decoder<'a>) -> Result<Self, crate::KeyError> {
        <Self as serde::Deserialize>::deserialize(decoder).context("Unable to decode timestamp")
    }
}

impl<'a> crate::StoreKey<'a> for core::marker::PhantomData<&'a ()> {
    fn encode_into(&self, _encoder: &mut crate::Encoder<'_>) -> Result<(), crate::KeyError> {
        Ok(())
    }

    fn decode_from(_decoder: &mut crate::Decoder<'a>) -> Result<Self, crate::KeyError> {
        Ok(Default::default())
    }
}
