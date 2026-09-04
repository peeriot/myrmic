//! Deploy-time spawn references.
//!
//! A cell that spawns children embeds one [`SpawnRef`] per referenced class in
//! its Wasm (via the SDK `declare!` macro). Each carries a magic sentinel, a
//! 32-byte hash slot (placeholder at compile time) and the referenced class
//! name. At deploy the toolchain [`scan_spawn_refs`] the module, resolves each
//! name to the child's SHA-256, and patches the hash slot in place. At runtime
//! the guest reads the patched bytes to spawn by content hash.

use alloc::string::String;
use alloc::vec::Vec;

/// Sentinel prefixing every embedded [`SpawnRef`]. Leading NUL keeps it from
/// colliding with ordinary string data.
pub const SPAWN_REF_MAGIC: [u8; 8] = [0x00, b'm', b'y', b'r', b'm', b'S', b'P', b'N'];

/// Upper bound on an embedded class name. Bounds the scan and rejects a
/// coincidental magic match whose trailing bytes are not a plausible name.
pub const SPAWN_REF_MAX_NAME: usize = 128;

/// Byte offset of the hash slot relative to the start of the magic.
const HASH_AT: usize = 8;
/// Byte offset of the name length relative to the start of the magic.
const NAME_LEN_AT: usize = HASH_AT + 32;
/// Byte offset of the name bytes relative to the start of the magic.
const NAME_AT: usize = NAME_LEN_AT + 4;

/// The embedded record. `#[repr(C)]` fixes the field order so the toolchain can
/// locate the hash slot at a constant offset from the magic.
#[repr(C)]
pub struct SpawnRef<const N: usize> {
    magic: [u8; 8],
    hash: [u8; 32],
    name_len: u32,
    name: [u8; N],
}

impl<const N: usize> SpawnRef<N> {
    /// Builds a record for `name`; `N` must equal `name.len()` (the SDK macro
    /// enforces this by passing `{ name.len() }` as the const generic).
    pub const fn new(name: &str) -> Self {
        let src = name.as_bytes();
        let mut buf = [0u8; N];
        let mut i = 0;
        while i < N {
            buf[i] = src[i];
            i += 1;
        }
        Self {
            magic: SPAWN_REF_MAGIC,
            hash: [0xAA; 32],
            name_len: N as u32,
            name: buf,
        }
    }

    /// Reference to the hash slot, for a runtime volatile read of the patched
    /// value.
    pub const fn hash_ref(&self) -> &[u8; 32] {
        &self.hash
    }
}

/// A spawn reference located in a Wasm module.
#[derive(Debug, PartialEq, Eq)]
pub struct FoundRef {
    /// The referenced class name.
    pub name: String,
    /// Byte offset in the module where the 32-byte hash slot begins.
    pub hash_offset: usize,
}

/// Finds every [`SpawnRef`] embedded in `wasm`, returning each referenced class
/// name and the offset of its hash slot (the 32 bytes to patch).
pub fn scan_spawn_refs(wasm: &[u8]) -> Vec<FoundRef> {
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(rel) = find(&wasm[search..], &SPAWN_REF_MAGIC) {
        let pos = search + rel;
        search = pos + 1;

        let name_at = pos + NAME_AT;
        if name_at > wasm.len() {
            break;
        }
        let name_len = u32::from_le_bytes([
            wasm[pos + NAME_LEN_AT],
            wasm[pos + NAME_LEN_AT + 1],
            wasm[pos + NAME_LEN_AT + 2],
            wasm[pos + NAME_LEN_AT + 3],
        ]) as usize;
        // A coincidental magic match won't have a plausible length + UTF-8 name;
        // skip it and keep scanning rather than trusting arbitrary bytes.
        if name_len == 0 || name_len > SPAWN_REF_MAX_NAME || name_at + name_len > wasm.len() {
            continue;
        }
        let Ok(name) = core::str::from_utf8(&wasm[name_at..name_at + name_len]) else {
            continue;
        };
        out.push(FoundRef {
            name: String::from(name),
            hash_offset: pos + HASH_AT,
        });
        search = name_at + name_len;
    }
    out
}

/// First offset of `needle` within `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_bytes<const N: usize>(r: &SpawnRef<N>) -> &[u8] {
        // SAFETY: #[repr(C)] POD; reading its own bytes.
        unsafe {
            core::slice::from_raw_parts(
                (r as *const SpawnRef<N>).cast::<u8>(),
                core::mem::size_of::<SpawnRef<N>>(),
            )
        }
    }

    #[test]
    fn scans_name_and_hash_offset_from_real_layout() {
        let r = SpawnRef::<5>::new("child");
        let mut buf = alloc::vec![0xEEu8; 7]; // leading noise
        buf.extend_from_slice(as_bytes(&r));

        let found = scan_spawn_refs(&buf);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "child");
        assert_eq!(
            &buf[found[0].hash_offset..found[0].hash_offset + 32],
            &[0xAA; 32]
        );
    }

    #[test]
    fn scans_multiple_refs() {
        let a = SpawnRef::<5>::new("child");
        let b = SpawnRef::<6>::new("worker");
        let mut buf = alloc::vec![];
        buf.extend_from_slice(as_bytes(&a));
        buf.extend_from_slice(&[0x11, 0x22, 0x33]); // gap
        buf.extend_from_slice(as_bytes(&b));

        let names: alloc::vec::Vec<_> = scan_spawn_refs(&buf).into_iter().map(|f| f.name).collect();

        assert_eq!(names, alloc::vec!["child", "worker"]);
    }

    #[test]
    fn ignores_stray_magic_with_implausible_length() {
        // The magic sentinel followed by a huge length must not be treated as a
        // ref (nor panic on an out-of-bounds slice).
        let mut buf = alloc::vec![];
        buf.extend_from_slice(&SPAWN_REF_MAGIC);
        buf.extend_from_slice(&[0xAA; 32]);
        buf.extend_from_slice(&u32::MAX.to_le_bytes()); // implausible name_len
        buf.extend_from_slice(b"noise");

        assert_eq!(scan_spawn_refs(&buf), alloc::vec![]);
    }
}
