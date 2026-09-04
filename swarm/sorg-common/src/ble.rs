//! Module for the types/functionality related to the BLE domain

use std::{fmt, str::FromStr};

use eui48::{MacAddress, MacAddressFormat};
use serde::{Deserialize, Serialize};

use crate::{Error, bail_validation, validation_err};

/// A strongly-typed wrapper for a BLE address parsed from a string
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BleAddress(String);

impl FromStr for BleAddress {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let mac = MacAddress::parse_str(s)
            .map_err(|_| validation_err!("invalid string provided as BLE Address: {s}"))?;

        if !mac.is_unicast() {
            bail_validation!("only unicast addresses are valid for BLE");
        }

        if mac.is_nil() {
            bail_validation!("BLE address must not be just nulls");
        }

        Ok(Self(mac.to_string(MacAddressFormat::Canonical)))
    }
}

impl fmt::Display for BleAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use claims::{assert_err, assert_ok};

    use crate::ble::BleAddress;

    #[test]
    fn valid_ble_address_parses_correctly() {
        let addr: BleAddress = assert_ok!("AA:BB:CC:DD:EE:FF".parse());
        assert_eq!(addr.to_string(), "aa-bb-cc-dd-ee-ff");
    }

    #[test]
    fn lowercase_and_dash_formats_are_accepted() {
        let addr1: BleAddress = assert_ok!("aa:bb:cc:dd:ee:ff".parse());
        let addr2: BleAddress = assert_ok!("aa-bb-cc-dd-ee-ff".parse());
        assert_eq!(addr1, addr2);
        assert_eq!(addr1.to_string(), "aa-bb-cc-dd-ee-ff");
    }

    #[test]
    fn rejects_multicast_addresses() {
        assert_err!("01:23:45:67:89:AB".parse::<BleAddress>());
    }

    #[test]
    fn rejects_all_zeros() {
        assert_err!("00:00:00:00:00:00".parse::<BleAddress>());
    }

    #[test]
    fn rejects_all_ones() {
        assert_err!("FF:FF:FF:FF:FF:FF".parse::<BleAddress>());
    }

    #[test]
    fn rejects_invalid_format() {
        assert_err!("invalid".parse::<BleAddress>());
    }
}
