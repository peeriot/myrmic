//! Metadata

use crc::{CRC_32_ISO_HDLC, Crc};
use serde::{Deserialize, Serialize};

/// Metadata structure
///
/// Helps us locate/detect length of AOT module bytes so we can reconstruct correctly the slice to
/// pass to WAMR
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    /// Magic number (should always be equal to 0x57414F54 'WAOT')
    pub magic: u32,
    /// Version of the metadata
    pub version: u32,
    /// Length of the AOT module
    pub len: u32,
    /// Checksum of AOT module
    ///
    /// Uses the [`CRC_32_ISO_HDLC`] algorithm (non-POSIX)
    pub crc: u32,
    /// SHA256 of the file (computed by remote) to avoid re-transfer of already locally stored
    /// modules
    pub hash: [u8; 32],
}

/// Magic number expected in metadata
pub const MAGIC: u32 = 0x5741_4F54; // 'WAOT'
/// Version of the metadata schema
pub const VERSION: u32 = 2;
/// The chosen CRC32 algorithm
///
/// The checksum of a file can be computed on Linux:
/// ```no_run
/// python3 -c 'import zlib,sys; c=0
/// f=open(sys.argv[1],"rb")
/// for b in iter(lambda:f.read(1<<20), b""): c=zlib.crc32(b,c)
/// print(f"0x{c&0xffffffff:08X}")' wasm_module.aot
/// ```
// CRC32 ISO HDLC is very widely available and supported. Fits best our use case
pub const CRC: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);
