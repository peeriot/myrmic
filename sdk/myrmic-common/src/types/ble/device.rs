//! BLE device identity and advertisement types, shared across host backends and the wasm cell
//! target.

use serde::{Deserialize, Serialize};

use super::Uuid;

/// BLE MAC Address
#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    postcard::experimental::max_size::MaxSize,
)]
pub struct Address {
    octets: [u8; 6],
    public: bool,
}

impl Address {
    /// Builds an address from its six octets in display order (`octets[0]` is
    /// the first byte shown, e.g. `C0` in `C0:98:E5:42:7A:11`) and whether it
    /// is a public or random address.
    pub const fn new(octets: [u8; 6], public: bool) -> Self {
        Self { octets, public }
    }

    /// The six address octets, in display order (`octets[0]` is the first
    /// byte shown, e.g. `C0` in `C0:98:E5:42:7A:11`).
    pub fn octets(&self) -> [u8; 6] {
        self.octets
    }

    /// Whether this is a public (`true`) or random (`false`) BLE address.
    pub fn is_public(&self) -> bool {
        self.public
    }
}

impl core::fmt::Display for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X} Type: {}",
            self.octets[0],
            self.octets[1],
            self.octets[2],
            self.octets[3],
            self.octets[4],
            self.octets[5],
            if self.public { "Public" } else { "Random" }
        )
    }
}

/// Maximum number of advertised service UUIDs carried on an [`Advertisement`].
///
/// A legacy advertisement realistically carries only one 128-bit service UUID, so this
/// bound is generous. UUIDs beyond it are dropped when building the advertisement.
pub const MAX_ADVERTISED_SERVICE_UUIDS: usize = 4;

/// A device discovered during scanning, but potentially not yet connected to
#[derive(Debug, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    /// Device address
    pub address: Address,
    /// Data parsed from the device's advertisement
    pub advertisement: Advertisement,
}

/// Data parsed from a device's BLE advertisement.
///
/// Every field here is derived from the advertisement packet, so a cell can inspect it
/// before deciding to connect, and the host can filter on it during scanning (see
/// `DiscoveryFilter`). More advertisement fields can be added over time.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Advertisement {
    /// Advertised Complete or Shortened Local Name, if present (capped at 32 bytes)
    pub local_name: Option<heapless::String<32>>,
    /// Advertised Manufacturer's Data, if present
    pub manufacturer_data: Option<ManufacturerData>,
    /// Advertised Service UUIDs (16-bit and 128-bit), capped at
    /// [`MAX_ADVERTISED_SERVICE_UUIDS`]
    pub service_uuids: heapless::Vec<Uuid, MAX_ADVERTISED_SERVICE_UUIDS>,
    /// First advertised Service Data element, if present
    pub service_data: Option<ServiceData>,
}

/// Manufacturer advertising data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManufacturerData {
    /// Company Identifier
    pub company_identifier: u16,
    /// Manufacturer data (supports only legacy advertisement)
    pub payload: heapless::Vec<u8, 27>,
}

/// Service advertising data (AD type 0x16 / 0x21: Service Data)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceData {
    /// UUID the data is associated with
    pub uuid: Uuid,
    /// Service data payload (supports only legacy advertisement)
    pub payload: heapless::Vec<u8, 27>,
}

/// BLE Security Settings
#[derive(Debug)]
pub enum SecuritySettings {
    /// Unsecured
    Unsecured,
    /// Authorize Pairing with the provided fixed passkey
    StaticPasskey(u32),
}

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use alloc::string::ToString;

    use super::*;
    use crate::{mac_addr_pub, mac_addr_rand};

    #[test]
    fn mac_addr_pub_builds_a_public_address() {
        const ADDR: Address = mac_addr_pub!("C0:98:E5:42:7A:11");

        assert_eq!(ADDR.octets(), [0xC0, 0x98, 0xE5, 0x42, 0x7A, 0x11]);
        assert!(ADDR.is_public());
        assert_eq!("C0:98:E5:42:7A:11 Type: Public", &ADDR.to_string());
    }

    #[test]
    fn mac_addr_rand_builds_a_random_address() {
        const ADDR: Address = mac_addr_rand!("DB:67:C1:B4:11:F2");

        assert_eq!(ADDR.octets(), [0xDB, 0x67, 0xC1, 0xB4, 0x11, 0xF2]);
        assert!(!ADDR.is_public());
        assert_eq!("DB:67:C1:B4:11:F2 Type: Random", &ADDR.to_string());
    }

    #[test]
    fn address_postcard_encoding_is_the_six_mac_bytes_plus_a_public_flag_byte() {
        // Pins the wire representation crossing the host/WASM boundary: a
        // fixed-size array serializes as its raw bytes with no length prefix,
        // followed by one byte for the bool.
        const ADDR: Address = mac_addr_pub!("C0:98:E5:42:7A:11");

        let bytes = postcard::to_allocvec(&ADDR).unwrap();
        assert_eq!(bytes, [0xC0, 0x98, 0xE5, 0x42, 0x7A, 0x11, 0x01]);

        let decoded: Address = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, ADDR);
    }
}
