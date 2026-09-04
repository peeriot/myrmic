//! Pipe module for Swarm Onboarding
//!
//! The Pipe module needs two pipes between the Device and the Installer:
//! - A pipe for sending data from the Installer to the Device
//! - A pipe for sending data from the Device to the Installer
//!
//! Each pipe is modeled with a pair of a `Read` + `Write` traits from the `embedded-io-async` crate.
//!
//! To implement the notion of messaging over the pipes, and yet - still keep the option to "stream" large messages
//! like the installation bundle, the messages are encoded in the following way:
//! - Each message is chunked into slices where each slice is no larger than 255 bytes
//! - Each slice is prefixed with a single byte indicating the length of the slice (0-255)
//! - Only the last slice of each message can be 0 bytes, and this slice indicates the end of the message stream (EOF)
//! - An empty message stream is therefore represented by a single 0 byte
//!
//! The pipes can then be mapped to a concrete link-layer transport.
//! For example, they have a trivial mapping to TCP socket, in that each TCP socket is essentially a `Read` + `Write` pair.
//!
//! For mapping the pipes to e.g. BLE+GATT, one can use e.g. a GATT Service with two characteristics:
//! - A characteristic for writing data to the Device which supports the `WriteRequest` GATT operation
//! - A characteristic for reading data from the Device which supports the `Indicate` GATT operation
//!
//! It is somewhat mandatory to use confirmed writes (as opposed to unconfirmed writes) as well as indications
//! as opposed to nitifications when implementing the pipses so as to make sure that GATT packets are not dropped
//! by the device when the installer is sending data to the device too quickly.

pub mod device;
pub mod installer;

mod io;
