//! Conversion between Modbus table content and the values cells see.
//!
//! Modbus only knows bits (coils, discrete inputs) and 16-bit words (input and
//! holding registers). A spec's `${type:name}` value says how to read them: a `bool`
//! is one bit, `u16`/`i16` one word, and `u32`/`i32`/`f32` two words. The entry's
//! [`ModbusByteOrder`] says how the bytes of a value lie in its words.

use sorg_common::{ModbusByteOrder, ModbusValueTemplate};

/// The raw content of the table entries a value spans, as read from or written to
/// the Modbus server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cells {
    Bits(Vec<bool>),
    Words(Vec<u16>),
}

/// A typed value, decoded from [`Cells`] or from a cell's JSON payload.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ModbusValue {
    Bool(bool),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    F32(f32),
}

/// How many table entries (bits or words) `template` spans.
pub(crate) fn quantity(template: &ModbusValueTemplate) -> u16 {
    match template {
        ModbusValueTemplate::Bool(_)
        | ModbusValueTemplate::U16(_)
        | ModbusValueTemplate::I16(_) => 1,
        ModbusValueTemplate::U32(_) | ModbusValueTemplate::I32(_) | ModbusValueTemplate::F32(_) => {
            2
        }
    }
}

impl ModbusValue {
    /// Decodes the entries read for `template`.
    ///
    /// A `NaN` or infinite `f32` is rejected: JSON has no such numbers, so a cell
    /// could not receive it in its `f32` field.
    pub(crate) fn decode(
        template: &ModbusValueTemplate,
        order: ModbusByteOrder,
        cells: &Cells,
    ) -> Result<Self, String> {
        let value = match (template, cells) {
            (ModbusValueTemplate::Bool(_), Cells::Bits(bits)) => match bits.as_slice() {
                [bit] => Self::Bool(*bit),
                _ => return Err(format!("expected 1 bit, got {}", bits.len())),
            },
            (ModbusValueTemplate::U16(_), Cells::Words(words)) => {
                Self::U16(one_word(words, order)?)
            }
            (ModbusValueTemplate::I16(_), Cells::Words(words)) => {
                Self::I16(one_word(words, order)?.cast_signed())
            }
            (ModbusValueTemplate::U32(_), Cells::Words(words)) => {
                Self::U32(two_words(words, order)?)
            }
            (ModbusValueTemplate::I32(_), Cells::Words(words)) => {
                Self::I32(two_words(words, order)?.cast_signed())
            }
            (ModbusValueTemplate::F32(_), Cells::Words(words)) => {
                let value = f32::from_bits(two_words(words, order)?);
                if !value.is_finite() {
                    return Err(format!("f32 value {value} has no JSON representation"));
                }
                Self::F32(value)
            }
            (template, cells) => {
                return Err(format!("cannot decode {template:?} from {cells:?}"));
            }
        };

        Ok(value)
    }

    /// The entries to write for this value.
    pub(crate) fn encode(self, order: ModbusByteOrder) -> Cells {
        match self {
            Self::Bool(bit) => Cells::Bits(vec![bit]),
            Self::U16(word) => Cells::Words(vec![swap_bytes(word, order)]),
            Self::I16(word) => Cells::Words(vec![swap_bytes(word.cast_unsigned(), order)]),
            Self::U32(value) => Cells::Words(split_words(value, order)),
            Self::I32(value) => Cells::Words(split_words(value.cast_unsigned(), order)),
            Self::F32(value) => Cells::Words(split_words(value.to_bits(), order)),
        }
    }

    /// Reads the value for `template` from a cell's JSON payload field.
    pub(crate) fn from_json(
        template: &ModbusValueTemplate,
        json: serde_json::Value,
    ) -> Result<Self, String> {
        fn from<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> Result<T, String> {
            serde_json::from_value(json).map_err(|err| err.to_string())
        }

        Ok(match template {
            ModbusValueTemplate::Bool(_) => Self::Bool(from(json)?),
            ModbusValueTemplate::U16(_) => Self::U16(from(json)?),
            ModbusValueTemplate::I16(_) => Self::I16(from(json)?),
            ModbusValueTemplate::U32(_) => Self::U32(from(json)?),
            ModbusValueTemplate::I32(_) => Self::I32(from(json)?),
            ModbusValueTemplate::F32(_) => Self::F32(from(json)?),
        })
    }

    /// The JSON a cell receives for this value. Only a value from [`Self::decode`]
    /// or [`Self::from_json`] goes here, so an `f32` is always finite.
    pub(crate) fn to_json(self) -> serde_json::Value {
        match self {
            Self::Bool(v) => v.into(),
            Self::U16(v) => v.into(),
            Self::I16(v) => v.into(),
            Self::U32(v) => v.into(),
            Self::I32(v) => v.into(),
            Self::F32(v) => serde_json::Number::from_f64(f64::from(v))
                .expect("a decoded f32 is finite")
                .into(),
        }
    }
}

/// Lays out a 16-bit value in its one register. Only the swap of the bytes
/// within a register applies: there is no second word to swap with.
fn swap_bytes(word: u16, order: ModbusByteOrder) -> u16 {
    match order {
        ModbusByteOrder::Abcd | ModbusByteOrder::Cdab => word,
        ModbusByteOrder::Badc | ModbusByteOrder::Dcba => word.swap_bytes(),
    }
}

fn one_word(words: &[u16], order: ModbusByteOrder) -> Result<u16, String> {
    match words {
        [word] => Ok(swap_bytes(*word, order)),
        _ => Err(format!("expected 1 register, got {}", words.len())),
    }
}

/// For each byte on the wire, in the order it is sent, which byte of the 32-bit
/// value it is - `0` the highest, `A`. Each layout is its own inverse: it also
/// says which byte on the wire each byte of the value is.
fn layout(order: ModbusByteOrder) -> [usize; 4] {
    match order {
        ModbusByteOrder::Abcd => [0, 1, 2, 3],
        ModbusByteOrder::Cdab => [2, 3, 0, 1],
        ModbusByteOrder::Badc => [1, 0, 3, 2],
        ModbusByteOrder::Dcba => [3, 2, 1, 0],
    }
}

fn two_words(words: &[u16], order: ModbusByteOrder) -> Result<u32, String> {
    let [first, second] = words else {
        return Err(format!("expected 2 registers, got {}", words.len()));
    };
    let [b0, b1] = first.to_be_bytes();
    let [b2, b3] = second.to_be_bytes();
    let wire = [b0, b1, b2, b3];

    Ok(u32::from_be_bytes(
        layout(order).map(|position| wire[position]),
    ))
}

fn split_words(value: u32, order: ModbusByteOrder) -> Vec<u16> {
    let value = value.to_be_bytes();
    let [b0, b1, b2, b3] = layout(order).map(|position| value[position]);

    vec![u16::from_be_bytes([b0, b1]), u16::from_be_bytes([b2, b3])]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABCD: ModbusByteOrder = ModbusByteOrder::Abcd;
    const ORDERS: [ModbusByteOrder; 4] = [
        ModbusByteOrder::Abcd,
        ModbusByteOrder::Cdab,
        ModbusByteOrder::Badc,
        ModbusByteOrder::Dcba,
    ];

    fn t(template: &str) -> ModbusValueTemplate {
        template.parse().unwrap()
    }

    #[test]
    fn bits_and_16_bit_values_span_one_entry_and_32_bit_values_two() {
        assert_eq!(quantity(&t("${bool:v}")), 1);
        assert_eq!(quantity(&t("${u16:v}")), 1);
        assert_eq!(quantity(&t("${i16:v}")), 1);
        assert_eq!(quantity(&t("${u32:v}")), 2);
        assert_eq!(quantity(&t("${i32:v}")), 2);
        assert_eq!(quantity(&t("${f32:v}")), 2);
    }

    #[test]
    fn decodes_a_coil() {
        let value = ModbusValue::decode(&t("${bool:on}"), ABCD, &Cells::Bits(vec![true]));
        assert_eq!(value, Ok(ModbusValue::Bool(true)));
    }

    #[test]
    fn decodes_signed_16_bit_registers_as_twos_complement() {
        let value = ModbusValue::decode(&t("${i16:v}"), ABCD, &Cells::Words(vec![0xFFFE]));
        assert_eq!(value, Ok(ModbusValue::I16(-2)));
    }

    #[test]
    fn byte_order_decides_where_each_byte_of_a_32_bit_value_lies() {
        // Bytes on the wire: 11 22 33 44.
        let words = Cells::Words(vec![0x1122, 0x3344]);

        for (order, expected) in [
            (ModbusByteOrder::Abcd, 0x1122_3344),
            (ModbusByteOrder::Cdab, 0x3344_1122),
            (ModbusByteOrder::Badc, 0x2211_4433),
            (ModbusByteOrder::Dcba, 0x4433_2211),
        ] {
            let value = ModbusValue::decode(&t("${u32:v}"), order, &words);
            assert_eq!(value, Ok(ModbusValue::U32(expected)), "{order:?}");
        }
    }

    #[test]
    fn a_byte_swap_also_applies_to_a_16_bit_value() {
        let words = Cells::Words(vec![0x1122]);

        for (order, expected) in [
            (ModbusByteOrder::Abcd, 0x1122),
            (ModbusByteOrder::Cdab, 0x1122),
            (ModbusByteOrder::Badc, 0x2211),
            (ModbusByteOrder::Dcba, 0x2211),
        ] {
            let value = ModbusValue::decode(&t("${u16:v}"), order, &words);
            assert_eq!(value, Ok(ModbusValue::U16(expected)), "{order:?}");
        }
    }

    #[test]
    fn decodes_an_ieee_754_float() {
        // 21.5 is 0x41AC_0000.
        let words = Cells::Words(vec![0x41AC, 0x0000]);
        let value = ModbusValue::decode(&t("${f32:celsius}"), ABCD, &words);
        assert_eq!(value, Ok(ModbusValue::F32(21.5)));
    }

    #[test]
    fn rejects_a_float_json_cannot_carry() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let words = ModbusValue::F32(value).encode(ABCD);
            let err = ModbusValue::decode(&t("${f32:celsius}"), ABCD, &words).unwrap_err();
            assert!(err.contains("no JSON representation"), "{err}");
        }
    }

    #[test]
    fn encoding_is_the_inverse_of_decoding() {
        let cases = [
            ("${bool:v}", ModbusValue::Bool(true)),
            ("${u16:v}", ModbusValue::U16(0xBEEF)),
            ("${i16:v}", ModbusValue::I16(-300)),
            ("${u32:v}", ModbusValue::U32(0xDEAD_BEEF)),
            ("${i32:v}", ModbusValue::I32(-70_000)),
            ("${f32:v}", ModbusValue::F32(-1.25)),
        ];

        for (template, value) in cases {
            for order in ORDERS {
                let cells = value.encode(order);
                assert_eq!(
                    ModbusValue::decode(&t(template), order, &cells),
                    Ok(value),
                    "{template} {order:?}"
                );
            }
        }
    }

    #[test]
    fn a_bit_template_does_not_decode_from_registers() {
        let err = ModbusValue::decode(&t("${bool:v}"), ABCD, &Cells::Words(vec![1])).unwrap_err();
        assert!(err.contains("cannot decode"), "{err}");
    }

    #[test]
    fn the_number_of_entries_must_match_the_type() {
        assert!(ModbusValue::decode(&t("${u32:v}"), ABCD, &Cells::Words(vec![1])).is_err());
        assert!(ModbusValue::decode(&t("${u16:v}"), ABCD, &Cells::Words(vec![1, 2])).is_err());
        assert!(ModbusValue::decode(&t("${bool:v}"), ABCD, &Cells::Bits(vec![])).is_err());
    }

    #[test]
    fn reads_a_value_from_json_in_the_templates_type() {
        let value = ModbusValue::from_json(&t("${i16:setpoint}"), serde_json::json!(-5));
        assert_eq!(value, Ok(ModbusValue::I16(-5)));

        // 70000 does not fit a u16.
        assert!(ModbusValue::from_json(&t("${u16:v}"), serde_json::json!(70_000)).is_err());
        assert!(ModbusValue::from_json(&t("${bool:v}"), serde_json::json!("yes")).is_err());
    }

    #[test]
    fn converts_values_to_json() {
        assert_eq!(ModbusValue::Bool(true).to_json(), serde_json::json!(true));
        assert_eq!(ModbusValue::I32(-7).to_json(), serde_json::json!(-7));
        assert_eq!(ModbusValue::F32(21.5).to_json(), serde_json::json!(21.5));
    }
}
