//! Helper macros

/// Macro allowing the easy declaration of a BLE MAC Public Address
///
/// # Usage
///
/// Since the implementation of this macro is solely relying on const contexts, this can be easily
/// used as such:
///
/// ```
/// use myrmic_common::{mac_addr_pub, types::ble::Address};
///
/// // Expected address of the sensor
/// const ADDR_SENSOR: Address = mac_addr_pub!("C0:98:E5:42:7A:11");
/// ```
#[macro_export]
macro_rules! mac_addr_pub {
    ($s:literal) => {{
        const BYTES: [u8; 6] = $crate::types::ble::parse_mac_str($s);
        $crate::types::ble::Address::new(BYTES, true)
    }};
}

/// Macro allowing the easy declaration of a BLE MAC Random Address
///
/// # Usage
///
/// Since the implementation of this macro is solely relying on const contexts, this can be easily
/// used as such:
///
/// ```
/// use myrmic_common::{mac_addr_rand, types::ble::Address};
///
/// // Expected address of the sensor
/// const ADDR_SENSOR: Address = mac_addr_rand!("DB:67:C1:B4:11:F2");
/// ```
#[macro_export]
macro_rules! mac_addr_rand {
    ($s:literal) => {{
        const BYTES: [u8; 6] = $crate::types::ble::parse_mac_str($s);
        $crate::types::ble::Address::new(BYTES, false)
    }};
}

/// Macro allowing the easy declaration of a 128-bit UUID
///
/// # Usage
///
/// Since the implementation of this macro is solely relying on const contexts, this can be easily
/// used as such:
///
/// ```
/// use myrmic_common::{uuid128, types::ble::Uuid};
///
/// const SERVICE_UUID: Uuid = uuid128!("00112233-4455-6677-8899-AABBCCDDEEFF");
/// ```
#[macro_export]
macro_rules! uuid128 {
    ($s:literal) => {{
        const BYTES: [u8; 16] = $crate::types::ble::parse_uuid_str($s);
        $crate::types::ble::Uuid::Bit128(BYTES)
    }};
}

/// Parses a MAC address from a "XX:XX:XX:XX:XX:XX" str (has to be length 17)
pub const fn parse_mac_str(s: &str) -> [u8; 6] {
    let b = s.as_bytes();

    // Right length
    if b.len() != 17 {
        panic!("MAC must be 17 bytes like XX:XX:XX:XX:XX:XX");
    }

    // Colons in the right places
    if !(b[2] == b':' && b[5] == b':' && b[8] == b':' && b[11] == b':' && b[14] == b':') {
        panic!("MAC must use ':' separators");
    }

    [
        hex_byte(b[0], b[1]),
        hex_byte(b[3], b[4]),
        hex_byte(b[6], b[7]),
        hex_byte(b[9], b[10]),
        hex_byte(b[12], b[13]),
        hex_byte(b[15], b[16]),
    ]
}

/// Parses a 128-bit UUID 8-4-4-4-12 str (has to be length 36)
pub const fn parse_uuid_str(s: &str) -> [u8; 16] {
    let b = s.as_bytes();

    // Right length
    if b.len() != 36 {
        panic!("UUID must be 36 bytes like 00112233-4455-6677-8899-AABBCCDDEEFF");
    }

    // Dashes in the right places
    if !(b[8] == b'-' && b[13] == b'-' && b[18] == b'-' && b[23] == b'-') {
        panic!("UUID must use '-' separators");
    }

    [
        hex_byte(b[0], b[1]),
        hex_byte(b[2], b[3]),
        hex_byte(b[4], b[5]),
        hex_byte(b[6], b[7]),
        hex_byte(b[9], b[10]),
        hex_byte(b[11], b[12]),
        hex_byte(b[14], b[15]),
        hex_byte(b[16], b[17]),
        hex_byte(b[19], b[20]),
        hex_byte(b[21], b[22]),
        hex_byte(b[24], b[25]),
        hex_byte(b[26], b[27]),
        hex_byte(b[28], b[29]),
        hex_byte(b[30], b[31]),
        hex_byte(b[32], b[33]),
        hex_byte(b[34], b[35]),
    ]
}

const fn hex_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => 10 + (b - b'a'),
        b'A'..=b'F' => 10 + (b - b'A'),
        _ => panic!("invalid hex digit in MAC"),
    }
}

const fn hex_byte(hi: u8, lo: u8) -> u8 {
    (hex_nibble(hi) << 4) | hex_nibble(lo)
}
