/*
 *
 *    Copyright (c) 2020-2022 Project CHIP Authors
 *
 *    Licensed under the Apache License, Version 2.0 (the "License");
 *    you may not use this file except in compliance with the License.
 *    You may obtain a copy of the License at
 *
 *        http://www.apache.org/licenses/LICENSE-2.0
 *
 *    Unless required by applicable law or agreed to in writing, software
 *    distributed under the License is distributed on an "AS IS" BASIS,
 *    WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 *    See the License for the specific language governing permissions and
 *    limitations under the License.
 */

// Derived from `rs-matter` 0.1.0, src/codec/base38.rs. Modified by Peeriot GmbH.
// See NOTICE for attribution and LICENSES/Apache-2.0.txt for the licence.

//! Base38 encoding and decoding functions.

#![allow(unused)]

use zenoh_traits::{Error, ErrorKind};

const BASE38_CHARS: [char; 38] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I',
    'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '-', '.',
];

const UNUSED: u8 = 255;

/// Map of base38 charater to numeric value
/// subtract 45 from the character, then index into this array, if possible
const DECODE_BASE38: [u8; 46] = [
    36,     // '-', =45
    37,     // '.', =46
    UNUSED, // '/', =47
    0,      // '0', =48
    1,      // '1', =49
    2,      // '2', =50
    3,      // '3', =51
    4,      // '4', =52
    5,      // '5', =53
    6,      // '6', =54
    7,      // '7', =55
    8,      // '8', =56
    9,      // '9', =57
    UNUSED, // ':', =58
    UNUSED, // ';', =59
    UNUSED, // '<', =50
    UNUSED, // '=', =61
    UNUSED, // '>', =62
    UNUSED, // '?', =63
    UNUSED, // '@', =64
    10,     // 'A', =65
    11,     // 'B', =66
    12,     // 'C', =67
    13,     // 'D', =68
    14,     // 'E', =69
    15,     // 'F', =70
    16,     // 'G', =71
    17,     // 'H', =72
    18,     // 'I', =73
    19,     // 'J', =74
    20,     // 'K', =75
    21,     // 'L', =76
    22,     // 'M', =77
    23,     // 'N', =78
    24,     // 'O', =79
    25,     // 'P', =80
    26,     // 'Q', =81
    27,     // 'R', =82
    28,     // 'S', =83
    29,     // 'T', =84
    30,     // 'U', =85
    31,     // 'V', =86
    32,     // 'W', =87
    33,     // 'X', =88
    34,     // 'Y', =89
    35,     // 'Z', =90
];

const RADIX: u32 = BASE38_CHARS.len() as u32;

/// An error indicating that invalid data was encountered during base38 decoding
#[derive(thiserror::Error, Debug, Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[error("invalid data encountered during base38 decoding")]
pub struct InvalidDataError;

impl Error for InvalidDataError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::InvalidData
    }
}

/// Encode a byte slice into a base38-encoded string
///
/// # Arguments
/// - `bytes` - byte slice to encode
///
/// # Returns
/// - An iterator over the base38-encoded characters
pub fn encode(bytes: &[u8]) -> impl Iterator<Item = char> + '_ {
    (0..bytes.len() / 3)
        .flat_map(move |index| {
            let offset = index * 3;

            encode_base38(
                ((bytes[offset + 2] as u32) << 16)
                    | ((bytes[offset + 1] as u32) << 8)
                    | (bytes[offset] as u32),
                5,
            )
        })
        .chain(
            core::iter::once(bytes.len() % 3).flat_map(move |remainder| {
                let offset = bytes.len() / 3 * 3;

                match remainder {
                    2 => encode_base38(
                        ((bytes[offset + 1] as u32) << 8) | (bytes[offset] as u32),
                        4,
                    ),
                    1 => encode_base38(bytes[offset] as u32, 2),
                    _ => encode_base38(0, 0),
                }
            }),
        )
}

fn encode_base38(mut value: u32, repeat: usize) -> impl Iterator<Item = char> {
    (0..repeat).map(move |_| {
        let remainder = value % RADIX;
        let c = BASE38_CHARS[remainder as usize];

        value = (value - remainder) / RADIX;

        c
    })
}

/// Decode a base38-encoded string into a byte slice
///
/// # Arguments
/// - `base38_str` - base38-encoded string to decode
///
/// # Returns
/// - An iterator over the decoded bytes or an `InvalidDataError` if decoding fails
pub fn decode(base38_str: &str) -> impl Iterator<Item = Result<u8, InvalidDataError>> + '_ {
    let stru = base38_str.as_bytes();

    (0..stru.len() / 5)
        .flat_map(move |index| {
            let offset = index * 5;
            decode_base38(&stru[offset..offset + 5])
        })
        .chain({
            let offset = stru.len() / 5 * 5;
            decode_base38(&stru[offset..])
        })
        .take_while(Result::is_ok)
}

fn decode_base38(chars: &[u8]) -> impl Iterator<Item = Result<u8, InvalidDataError>> {
    let mut value = 0u32;
    let mut cerr = None;

    let repeat = match chars.len() {
        5 => 3,
        4 => 2,
        2 => 1,
        0 => 0,
        _ => -1,
    };

    if repeat >= 0 {
        for c in chars.iter().rev() {
            match decode_char(*c) {
                Ok(v) => value = value * RADIX + v as u32,
                Err(err) => {
                    cerr = Some(err);
                    break;
                }
            }
        }
    } else {
        cerr = Some(InvalidDataError)
    }

    (0..repeat)
        .map(move |_| {
            if let Some(err) = cerr {
                Err(err)
            } else {
                let byte = (value & 0xff) as u8;

                value >>= 8;

                Ok(byte)
            }
        })
        .take_while(Result::is_ok)
}

fn decode_char(c: u8) -> Result<u8, InvalidDataError> {
    if !(45..=90).contains(&c) {
        Err(InvalidDataError)?;
    }

    let c = DECODE_BASE38[c as usize - 45];
    if c == UNUSED {
        Err(InvalidDataError)?;
    }

    Ok(c)
}

#[cfg(test)]
mod tests {
    use crate::utils::slicebuf::SliceBuf;

    use super::*;

    extern crate alloc;

    const ENCODED: &str = "-MOA57ZU02IT2L2BJ00";
    const DECODED: &[u8] = &[
        0x88, 0xff, 0xa7, 0x91, 0x50, 0x40, 0x00, 0x47, 0x51, 0xdd, 0x02,
    ];

    #[test]
    fn test_encode() {
        let mut buf = [0; 100];
        let mut buf = SliceBuf::new(&mut buf);

        buf.extend(encode(DECODED).map(|c| c as u8)).unwrap();

        assert_eq!(buf.split_str().unwrap().0, ENCODED);
    }

    #[test]
    fn test_decode() {
        let mut buf = [0; 100];
        let mut buf = SliceBuf::new(&mut buf);

        buf.extend(decode(ENCODED).map(|r| r.unwrap())).unwrap();

        assert_eq!(buf.split().0, DECODED);
    }
}
