//! Wire types: Request, Response, and server/client-side result enums.

use serde::{Deserialize, Serialize};

/// Versioned request sent by the client.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Hello {
        protocol_version: u16,
    },
    TapResolve {
        name: String,
    },
    TapReadRetained {
        handle: u32,
    },
    TapTakeEvent {
        handle: u32,
    },
    TapDrainBatch {
        handle: u32,
    },
    TapListLen,
    TapListEntry {
        index: u32,
    },
    OutletResolve {
        name: String,
    },
    OutletWrite {
        handle: u32,
        bytes: Vec<u8>,
    },
    // Appended after v1 (postcard encodes variants by index, so new variants
    // must go at the end; the reserved outlet slots above kept their position).
    OutletListLen,
    OutletListEntry {
        index: u32,
    },
    /// Declared wire type of a tap slot (swarm#1315).
    TapTypeId {
        handle: u32,
    },
    /// Declared command type of an outlet slot (swarm#1315).
    OutletTypeId {
        handle: u32,
    },
}

/// Response sent by the server. Variants are deliberately unprefixed —
/// they are result shapes shared across op families.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Response {
    HelloOk {
        version: u16,
    },
    HelloRejected {
        supported_version: u16,
    },
    Handle {
        handle: u32,
    },
    NotFound,
    Retained {
        timestamp_ms: u64,
        bytes: Vec<u8>,
    },
    Event {
        bytes: Vec<u8>,
    },
    Empty,
    InvalidHandle,
    Count {
        count: u32,
    },
    Entry {
        name: String,
        kind: u8,
    },
    OutOfRange,
    /// Flat top-level variant for reserved/unimplemented operations (SR-19) —
    /// still answered for outlet requests when the server has no outlet store.
    Unsupported,
    // Appended after v1 (postcard encodes variants by index).
    /// An outlet write was accepted and stored.
    Written,
    /// An outlet write was refused because the payload does not decode into
    /// the outlet's declared command type (OUT-08).
    Rejected,
    /// A slot's declared wire type (`WireType::TYPE_ID`, swarm#1315).
    TypeId {
        id: u32,
    },
}

/// Server-side result of a read operation.
#[derive(Debug, PartialEq)]
pub enum StoreRead {
    Value { timestamp_ms: u64, bytes: Vec<u8> },
    Empty,
    InvalidHandle,
}

/// Client-side result of a read operation (maps IPC-down to `Unavailable`).
#[derive(Debug, PartialEq)]
pub enum ClientRead {
    Value { timestamp_ms: u64, bytes: Vec<u8> },
    Empty,
    Unavailable,
}

/// Server-side result of an outlet write.
#[derive(Debug, PartialEq)]
pub enum StoreWrite {
    /// The command was decoded and stored as the outlet's latest value.
    Ok,
    /// The payload does not decode into the outlet's declared type (OUT-08).
    Rejected,
    InvalidHandle,
}

/// Client-side result of an outlet write (maps IPC-down to `Unavailable`).
#[derive(Debug, PartialEq)]
pub enum ClientWrite {
    Ok,
    /// The server refused the payload (wrong declared type).
    Rejected,
    Unavailable,
}

/// Server-side seam: generated code and test stubs implement this.
/// Server handles are ≥ 1; 0 is reserved invalid.
pub trait TapStore: Send + Sync + 'static {
    fn resolve(&self, name: &str) -> Option<u32>;
    fn read_retained(&self, h: u32) -> StoreRead;
    fn take_event(&self, h: u32) -> StoreRead;
    fn list_len(&self) -> u32;
    fn list_entry(&self, index: u32) -> Option<(String, u8)>;
    /// The slot's declared wire type, or `None` for an unknown handle.
    fn type_id(&self, h: u32) -> Option<u32>;
}

/// Server-side seam for the outlet registry — the write-side mirror of
/// [`TapStore`]. Implemented by generated code (over `OutletRegistry`) and
/// test stubs. Writes stamp the value with the server process's clock.
pub trait OutletStore: Send + Sync + 'static {
    fn resolve(&self, name: &str) -> Option<u32>;
    fn write(&self, h: u32, bytes: &[u8]) -> StoreWrite;
    fn list_len(&self) -> u32;
    fn list_entry(&self, index: u32) -> Option<(String, u8)>;
    /// The outlet's declared command type, or `None` for an unknown handle.
    fn type_id(&self, h: u32) -> Option<u32>;
}
