// Derived from `storekey` 0.6.0, src/decode.rs. Modified by Peeriot GmbH.
// Upstream carries no copyright notice in this file; see NOTICE for
// attribution and LICENSES/Apache-2.0.txt for the licence.

use byteorder::{BE, ReadBytesExt};

use serde::de::Visitor;
use std::marker::PhantomData;
use std::str;

use self::read::{ReadReference, Reference};

pub mod read;

/// A decoder for deserializing bytes from an order preserving format to a value.
#[derive(Debug)]
pub struct Deserializer<R> {
    reader: R,
}

/// Errors that may be occur when deserializing.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("storekey is not a self-describing format")]
    DeserializeAnyUnsupported,
    #[error("Encountered unexpected EOF when deserializing UTF8")]
    UnexpectedEof,
    #[error("Attempted to deserialize invalid UTF8")]
    InvalidUtf8(#[from] std::str::Utf8Error),
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl serde::de::Error for Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Error::Message(msg.to_string())
    }
}

/// Shorthand for `Result<T, storekey::de::Error>`.
pub type Result<T> = std::result::Result<T, Error>;

impl<'de, R: ReadReference<'de>> Deserializer<R> {
    /// Creates a new ordered bytes encoder whose output will be written to the provided writer.
    pub fn new(reader: R) -> Deserializer<R> {
        Deserializer { reader }
    }

    pub fn move_on(&mut self) -> Result<bool> {
        let buf = self.reader.fill_buf()?;
        match buf.first() {
            Some(v) if v == &0x01 => {
                self.reader.consume(1);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Deserialize a `u64` that has been serialized using the `serialize_var_u64` method.
    pub fn deserialize_var_u64(&mut self) -> Result<u64> {
        let header = self.reader.read_u8()?;
        let n = header >> 4;
        let (mut val, _) = u64::from(header & 0x0F).overflowing_shl(u32::from(n) * 8);
        for i in 1..=n {
            let byte = self.reader.read_u8()?;
            val += u64::from(byte) << ((n - i) * 8);
        }
        Ok(val)
    }

    /// Deserialize an `i64` that has been serialized using the `serialize_var_i64` method.
    pub fn deserialize_var_i64(&mut self) -> Result<i64> {
        let header = self.reader.read_u8()?;
        let mask = ((header ^ 0x80).cast_signed() >> 7).cast_unsigned();
        let n = ((header >> 3) ^ mask) & 0x0F;
        let (mut val, _) = u64::from((header ^ mask) & 0x07).overflowing_shl(u32::from(n) * 8);
        for i in 1..=n {
            let byte = self.reader.read_u8()?;
            val += u64::from(byte ^ mask) << ((n - i) * 8);
        }
        let final_mask = ((i64::from(mask) << 63) >> 63).cast_unsigned();
        val ^= final_mask;
        Ok(val.cast_signed())
    }
}

impl<'de, R> serde::de::Deserializer<'de> for &mut Deserializer<R>
where
    R: ReadReference<'de>,
{
    type Error = Error;

    fn is_human_readable(&self) -> bool {
        false
    }

    fn deserialize_any<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        Err(Error::DeserializeAnyUnsupported)
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let b = self.reader.read_u8()? != 0;
        visitor.visit_bool(b)
    }

    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let i = self.reader.read_i8()?;
        visitor.visit_i8(i ^ i8::MIN)
    }

    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let i = self.reader.read_i16::<BE>()?;
        visitor.visit_i16(i ^ i16::MIN)
    }

    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let i = self.reader.read_i32::<BE>()?;
        visitor.visit_i32(i ^ i32::MIN)
    }

    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let i = self.reader.read_i64::<BE>()?;
        visitor.visit_i64(i ^ i64::MIN)
    }

    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let u = self.reader.read_u8()?;
        visitor.visit_u8(u)
    }

    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let u = self.reader.read_u16::<BE>()?;
        visitor.visit_u16(u)
    }

    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let u = self.reader.read_u32::<BE>()?;
        visitor.visit_u32(u)
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let u = self.reader.read_u64::<BE>()?;
        visitor.visit_u64(u)
    }

    fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let val = self.reader.read_i32::<BE>()?;
        let t = ((val ^ i32::MIN) >> 31) | i32::MIN;
        let f: f32 = f32::from_bits((val ^ t).cast_unsigned());
        visitor.visit_f32(f)
    }

    fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let val = self.reader.read_i64::<BE>()?;
        let t = ((val ^ i64::MIN) >> 63) | i64::MIN;
        let f: f64 = f64::from_bits((val ^ t).cast_unsigned());
        visitor.visit_f64(f)
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.reader.read_reference_until(0u8) {
            Ok(reference) => {
                let bytes = match reference {
                    Reference::Borrowed(b) | Reference::Copied(b) => b,
                };
                let string = std::str::from_utf8(bytes)?;
                let c = string.chars().next().ok_or_else(|| {
                    Error::Message("expected a single character, found empty input".to_owned())
                })?;
                visitor.visit_char(c)
            }
            Err(_) => Err(Error::UnexpectedEof),
        }
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.reader.read_reference_until(0u8) {
            Ok(reference) => match reference {
                Reference::Borrowed(bytes) => {
                    let string = std::str::from_utf8(bytes)?;
                    visitor.visit_borrowed_str(string)
                }
                Reference::Copied(bytes) => {
                    let string = std::str::from_utf8(bytes)?;
                    visitor.visit_str(string)
                }
            },
            Err(_) => Err(Error::UnexpectedEof),
        }
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let len = self.reader.read_u64::<BE>()?;
        let len = usize::try_from(len)
            .map_err(|e| Error::Message(format!("byte length exceeds usize range: {e}")))?;
        match self.reader.read_reference(len)? {
            Reference::Borrowed(bytes) => visitor.visit_borrowed_bytes(bytes),
            Reference::Copied(bytes) => visitor.visit_bytes(bytes),
        }
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let len = self.reader.read_u64::<BE>()?;
        let len = usize::try_from(len)
            .map_err(|e| Error::Message(format!("byte length exceeds usize range: {e}")))?;
        let bytes = match self.reader.read_reference(len)? {
            Reference::Borrowed(bytes) | Reference::Copied(bytes) => bytes,
        };
        visitor.visit_byte_buf(bytes.into())
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.reader.read_u8()? {
            0 => visitor.visit_none(),
            1 => visitor.visit_some(&mut *self),
            b => {
                let msg = format!("expected `0` or `1` for option tag - found {}", b);
                Err(Error::Message(msg))
            }
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V>(self, _name: &'static str, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V>(self, _name: &'static str, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        struct Access<'de, 'a, R>
        where
            R: ReadReference<'de>,
        {
            deserializer: &'a mut Deserializer<R>,
            _spooky: PhantomData<&'de ()>,
        }

        impl<'de, R> serde::de::SeqAccess<'de> for Access<'de, '_, R>
        where
            R: ReadReference<'de>,
        {
            type Error = Error;

            fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>>
            where
                T: serde::de::DeserializeSeed<'de>,
            {
                if self.deserializer.move_on()? {
                    return Ok(None);
                }
                match serde::de::DeserializeSeed::deserialize(seed, &mut *self.deserializer) {
                    Ok(v) => Ok(Some(v)),
                    Err(Error::Io(ref err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                        Ok(None)
                    }
                    Err(err) => Err(err),
                }
            }
        }

        visitor.visit_seq(Access {
            deserializer: self,
            _spooky: PhantomData,
        })
    }

    fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        struct Access<'de, 'a, R>
        where
            R: ReadReference<'de>,
        {
            deserializer: &'a mut Deserializer<R>,
            len: usize,
            _spooky: PhantomData<&'de ()>,
        }

        impl<'de, R> serde::de::SeqAccess<'de> for Access<'de, '_, R>
        where
            R: ReadReference<'de>,
        {
            type Error = Error;

            fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>>
            where
                T: serde::de::DeserializeSeed<'de>,
            {
                if self.len == 0 {
                    return Ok(None);
                }
                self.len -= 1;
                let value = serde::de::DeserializeSeed::deserialize(seed, &mut *self.deserializer)?;
                Ok(Some(value))
            }

            fn size_hint(&self) -> Option<usize> {
                Some(self.len)
            }
        }

        visitor.visit_seq(Access {
            deserializer: self,
            len,
            _spooky: PhantomData,
        })
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        struct Access<'de, 'a, R>
        where
            R: ReadReference<'de>,
        {
            deserializer: &'a mut Deserializer<R>,
            _spooky: PhantomData<&'de ()>,
        }

        impl<'de, R> serde::de::MapAccess<'de> for Access<'de, '_, R>
        where
            R: ReadReference<'de>,
        {
            type Error = Error;

            fn next_key_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>>
            where
                T: serde::de::DeserializeSeed<'de>,
            {
                if self.deserializer.move_on()? {
                    return Ok(None);
                }
                match serde::de::DeserializeSeed::deserialize(seed, &mut *self.deserializer) {
                    Ok(v) => Ok(Some(v)),
                    Err(Error::Io(ref err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                        Ok(None)
                    }
                    Err(err) => Err(err),
                }
            }

            fn next_value_seed<T>(&mut self, seed: T) -> Result<T::Value>
            where
                T: serde::de::DeserializeSeed<'de>,
            {
                serde::de::DeserializeSeed::deserialize(seed, &mut *self.deserializer)
            }
        }

        visitor.visit_map(Access {
            deserializer: self,
            _spooky: PhantomData,
        })
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_tuple(fields.len(), visitor)
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        visitor.visit_enum(self)
    }

    fn deserialize_ignored_any<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(Error::DeserializeAnyUnsupported)
    }

    fn deserialize_identifier<V>(self, _visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(Error::DeserializeAnyUnsupported)
    }
}

impl<'de, R> serde::de::EnumAccess<'de> for &mut Deserializer<R>
where
    R: ReadReference<'de>,
{
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant)>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        let idx: u32 = serde::de::Deserialize::deserialize(&mut *self)?;
        let val: Result<_> = seed.deserialize(serde::de::IntoDeserializer::into_deserializer(idx));
        Ok((val?, self))
    }
}

impl<'de, R> serde::de::VariantAccess<'de> for &mut Deserializer<R>
where
    R: ReadReference<'de>,
{
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        Ok(())
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value>
    where
        T: serde::de::DeserializeSeed<'de>,
    {
        serde::de::DeserializeSeed::deserialize(seed, self)
    }

    fn tuple_variant<V>(self, len: usize, visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        serde::de::Deserializer::deserialize_tuple(self, len, visitor)
    }

    fn struct_variant<V>(self, fields: &'static [&'static str], visitor: V) -> Result<V::Value>
    where
        V: serde::de::Visitor<'de>,
    {
        serde::de::Deserializer::deserialize_tuple(self, fields.len(), visitor)
    }
}
