//! Data structures describing an onboarding request payload as well as the onboarding topic name

use core::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

/// A topic where the onboarding plugin expects to receive onboarding
/// requests from the UI
pub const ONBOARDING_REQUEST_TOPIC: &str = "@onboarding/@v1/@-request";

/// An onboarding request sent by the UI to the onboarding plugin
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnboardingRequest {
    /// The operational networks to configure on the device
    pub operational_networks: Vec<Network>,
    /// The device QR code content
    pub device_qr: String,
}

/// IP network configuration
/// For now, only IPv4 is supported
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum IpNetwork {
    /// `DHCPv4` configuration
    Dhcpv4,
    /// Static IPv4 configuration
    FixedIpv4 {
        /// The static IP address
        ip: Ipv4Addr,
        /// The gateway address and the subnet
        gateway: Ipv4Addr,
        /// The subnet mask length
        netmask: u8,
        /// An optional set of DNS servers
        dns: Vec<Ipv4Addr>,
    },
}

/// Network configuration for the device
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Network {
    /// Wi-Fi network
    Wifi {
        /// The SSID of the Wi-Fi network
        ssid: String,
        /// The password of the Wi-Fi network (TODO: change to `Vec<u8>`?)
        /// If empty, assumes an open network
        password: Option<String>,
        /// The IP network configuration
        ip_network: IpNetwork,
    },
    /// Ethernet network
    Ethernet {
        /// The IP network configuration
        ip_network: IpNetwork,
    },
    /// Bluetooth network
    Ble,
}
