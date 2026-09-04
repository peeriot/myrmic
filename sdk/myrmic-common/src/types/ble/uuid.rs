//! BLE UUID Types

use serde::{Deserialize, Serialize};

/// A BLE UUID
#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    postcard::experimental::max_size::MaxSize,
)]
pub enum Uuid {
    /// A 128-bit UUID
    Bit128([u8; 16]),
    /// A 16-bit UUID
    Bit16(u16),
}

impl From<[u8; 16]> for Uuid {
    fn from(value: [u8; 16]) -> Self {
        Self::Bit128(value)
    }
}

impl From<u16> for Uuid {
    fn from(value: u16) -> Self {
        Self::Bit16(value)
    }
}

impl core::fmt::Display for Uuid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Bit128(value) => {
                // 8-4-4-4-12 bit-pattern
                for (idx, byte) in value.iter().enumerate() {
                    match idx {
                        4 | 6 | 8 | 10 => write!(f, "-{:02X}", byte)?,
                        _ => write!(f, "{:02X}", byte)?,
                    }
                }

                Ok(())
            }
            Self::Bit16(value) => write!(f, "{:#06X}", value),
        }
    }
}

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use alloc::string::ToString;

    use super::*;
    use crate::uuid128;

    #[test]
    fn display_128() {
        let uuid = Uuid::Bit128([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ]);
        assert_eq!("00112233-4455-6677-8899-AABBCCDDEEFF", &uuid.to_string());
    }

    #[test]
    fn display_16() {
        let uuid = Uuid::Bit16(0x00AA);
        assert_eq!("0x00AA", &uuid.to_string());
    }

    #[test]
    fn const_macro() {
        const UUID: Uuid = uuid128!("00112233-4455-6677-8899-AABBCCDDEEFF");

        assert_eq!("00112233-4455-6677-8899-AABBCCDDEEFF", &UUID.to_string());
    }

    #[test]
    fn bit128_postcard_encoding_is_a_variant_tag_plus_the_raw_bytes() {
        // Pins the wire representation crossing the host/WASM boundary: a
        // fixed-size array serializes as its raw bytes with no length prefix.
        let uuid = Uuid::Bit128([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ]);

        let bytes = postcard::to_allocvec(&uuid).unwrap();
        assert_eq!(
            bytes,
            [
                0x00, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC,
                0xDD, 0xEE, 0xFF,
            ]
        );

        let decoded: Uuid = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, uuid);
    }

    #[test]
    fn bit16_postcard_encoding_is_a_variant_tag_plus_a_varint() {
        let uuid = Uuid::Bit16(0x00AA);

        let bytes = postcard::to_allocvec(&uuid).unwrap();
        assert_eq!(bytes, [0x01, 0xAA, 0x01]);

        let decoded: Uuid = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, uuid);
    }
}
