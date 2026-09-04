//! Wire protocol between the orchestrator and node workers.
//!
//! Frames are length-prefixed (u32 LE) postcard payloads — the same encoding
//! production uses for `ReplicaMessage` over zenoh.

use db_commons::models::Scope;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub type NodeId = [u8; 16];
pub type TxId = u64;

/// Frames larger than this indicate a corrupted stream, not a real message.
const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Op {
    Put {
        scope: Scope,
        key: String,
        payload: String,
    },
    Del {
        scope: Scope,
        key: String,
    },
}

impl Op {
    pub fn scope(&self) -> &Scope {
        match self {
            Op::Put { scope, .. } | Op::Del { scope, .. } => scope,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            Op::Put { key, .. } | Op::Del { key, .. } => key,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxSpec {
    pub id: TxId,
    pub ops: Vec<Op>,
    pub retention_ms: Option<u64>,
}

/// Orchestrator → worker.
#[derive(Debug, Serialize, Deserialize)]
pub enum ToWorker {
    /// Run one write transaction (spawned; many can be in flight at once).
    RunTx(TxSpec),
    /// Read back every key/value in the scope.
    Dump { scope: Scope },
    /// Report the node's replication frontier.
    Heads,
    /// Trigger a replication announce.
    Announce,
    /// A `ReplicaMessage` published by another node (postcard-encoded).
    Replica { from: NodeId, payload: Vec<u8> },
    /// Graceful stop (scenario teardown — kills don't use this).
    Shutdown,
}

/// Worker → orchestrator.
#[derive(Debug, Serialize, Deserialize)]
pub enum ToParent {
    Hello {
        name: String,
        node_id: NodeId,
        pid: u32,
    },
    TxResult {
        id: TxId,
        /// The tx's HLC time — also the version every put/delete was written at.
        ts: u64,
        ok: bool,
        error: Option<String>,
    },
    DumpResult {
        scope: Scope,
        /// key → value, where values are the harness's `txid:ts:payload` strings.
        entries: Vec<(String, String)>,
    },
    HeadsResult {
        heads: Vec<HeadEntry>,
    },
    /// A `ReplicaMessage` this node published (postcard-encoded).
    Replica {
        payload: Vec<u8>,
    },
    /// Ack of `Shutdown`.
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadEntry {
    pub scope: Scope,
    pub epoch: u64,
    pub ts: u64,
    pub node: NodeId,
    pub deletion: bool,
}

impl HeadEntry {
    /// Sort key — `Scope` itself doesn't implement `Ord`.
    pub fn sort_key(&self) -> (String, String, String, u64, u64, NodeId, bool) {
        (
            self.scope.namespace.clone(),
            self.scope.database.clone(),
            self.scope.schema.clone(),
            self.ts,
            self.epoch,
            self.node,
            self.deletion,
        )
    }
}

pub async fn write_frame<T, W>(w: &mut W, msg: &T) -> anyhow::Result<()>
where
    T: Serialize,
    W: AsyncWrite + Unpin,
{
    let bytes = postcard::to_allocvec(msg)?;
    w.write_all(&u32::try_from(bytes.len())?.to_le_bytes())
        .await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<T, R>(r: &mut R) -> anyhow::Result<T>
where
    T: DeserializeOwned,
    R: AsyncRead + Unpin,
{
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(len <= MAX_FRAME, "frame too large: {len}");
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}

/// Environment variables that configure a worker process.
pub mod env {
    /// Presence selects worker mode; value is the node name.
    pub const NAME: &str = "DB_DURABILITY_WORKER";
    pub const SOCKET: &str = "DB_DURABILITY_SOCKET";
    pub const DIR: &str = "DB_DURABILITY_DIR";
    pub const NAMESPACE: &str = "DB_DURABILITY_NAMESPACE";
    pub const GC_MS: &str = "DB_DURABILITY_GC_MS";
    pub const LOG: &str = "DB_DURABILITY_LOG";
}
