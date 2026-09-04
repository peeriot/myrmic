use core::time::Duration;

use crate::models::{Epoch, NodeId, RawKey, Scope, SyncPointId, TxId, Value, Version};

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub use vecmap::VecMap;

#[cfg(feature = "std")]
pub use wire::{read_snapshot, write_snapshot};

#[cfg(feature = "std")]
mod wire;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum ReplicaMessage {
    /// Used to determine who is out there.
    Probe(Probe),
    /// This message is used to announce to other replicas what is currently known by this replica.
    Announce(Announce),
    /// This is used when a replica has noticed it's drifted out of sync with the others, and is requesting a catchup.
    ChangeSetReq(ChangeSetReq),
    /// This is the changeset between two sync points.
    ChangeSet(ChangeSet),
}

impl ReplicaMessage {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Probe(_) => "PROBE",
            Self::Announce(_) => "ANNOUNCE",
            Self::ChangeSetReq(_) => "CHANGESET_REQ",
            Self::ChangeSet(_) => "CHANGESET",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Probe {
    pub filter: Vec<Scope>,
}

pub type ScopeFrontier = BTreeMap<Version, (Epoch, NodeId)>;

pub type Fingerprint = u64;

/// One scope's announced inventory: heads at or below `baseline` (in both ts
/// and epoch) are elided from `heads` and XOR-folded into `fingerprint`
/// instead, so a receiver verifies the shared prefix by folding its own heads
/// at the same cut and comparing.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ScopeAnnounce {
    pub baseline: Option<Version>,
    pub fingerprint: Fingerprint,
    /// Every head above the baseline, plus older heads whose epoch outruns it
    /// (deletes/restores still propagating).
    pub heads: ScopeFrontier,
}

impl ScopeAnnounce {
    pub fn full(heads: ScopeFrontier) -> Self {
        Self {
            baseline: None,
            fingerprint: 0,
            heads,
        }
    }

    pub fn elides(&self, ts: Version, epoch: Epoch) -> bool {
        self.baseline.is_some_and(|b| ts <= b && epoch <= b)
    }

    /// The newest version this announce vouches for. Elided heads never
    /// exceed the baseline, so the maximum of both parts is exact.
    pub fn head(&self) -> Option<Version> {
        let explicit = self.heads.keys().next_back().copied();
        explicit.max(self.baseline)
    }
}

/// Order-independent head digest: XOR [`head_fingerprint`] over a set of
/// heads and equal folds mean equal sets (up to hash collisions).
pub fn head_fingerprint(ts: Version, epoch: Epoch, node: &NodeId) -> Fingerprint {
    fn mix(mut x: u64) -> u64 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }

    let (lo, hi) = node.split_at(8);
    let lo = u64::from_le_bytes(lo.try_into().expect("nodeid is 16 bytes"));
    let hi = u64::from_le_bytes(hi.try_into().expect("nodeid is 16 bytes"));

    mix(mix(mix(mix(ts) ^ epoch) ^ lo) ^ hi)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Announce {
    pub known: VecMap<Scope, ScopeAnnounce>,
    /// Whether the sender durably retains what it announces (a full replica)
    /// rather than merely serving it out for others to pull (an offloader).
    /// Offload retirement keys off this: an offloader only retires once a full
    /// replica reports covering it, never off another offloader's coverage.
    pub full_replica: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ChangeSetReq {
    pub tx_id: Option<TxId>,
    pub scope: Scope,
    /// Inclusive cursor: receiver claims to already have every sync point with
    /// `ts <= since_ts` at its latest known epoch, **except** as overridden by
    /// [`Self::epoch_floors`].
    pub since_ts: Option<Version>,
    /// Per-ts epoch floors. The sender must send any local sync point at one of
    /// these timestamps only if its `epoch` is strictly greater than the floor.
    /// Takes precedence over `since_ts`.
    pub epoch_floors: BTreeMap<Version, Epoch>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ChangeSet {
    pub tx_id: Option<TxId>,
    pub scope: Scope,
    /// One or more sync-point chunks batched into a single message, so serving
    /// a catch-up costs a bounded number of messages rather than one per point.
    pub chunks: Vec<Chunk>,
}

pub type Snapshot = Vec<Chunk>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Chunk {
    pub id: SyncPointId,
    pub meta: SyncMeta,
    pub entries: Vec<(RawKey, Option<Value>)>,
}

/// Direct catch-up between two nodes, served over a queryable instead of the
/// broadcast announce/changeset gossip. The requester paces: each pull request
/// returns one bounded page and a resume cursor, so a transfer survives slow
/// links, never floods uninvolved peers, and resumes where it stopped. The
/// same channel answers coverage checks, which is how a draining offloader
/// verifies a replica holds everything it does before retiring.
pub mod sync {
    use super::{Chunk, Epoch, Scope, SyncPointId, Version};
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub enum Request {
        Pull(PullRequest),
        Verify(VerifyRequest),
    }

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub enum Response {
        Pull(PullResponse),
        Verify(VerifyResponse),
    }

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct PullRequest {
        pub scope: Scope,
        /// Resume cursor: serve strictly after this sync point, in key order.
        pub after: Option<SyncPointId>,
        /// As in [`super::ChangeSetReq`]: the requester already holds every
        /// sync point with `ts <= since_ts`, except as overridden by
        /// [`Self::epoch_floors`].
        pub since_ts: Option<Version>,
        /// Per-ts epoch floors; send a point at one of these timestamps only
        /// if its epoch is strictly greater. Takes precedence over `since_ts`.
        pub epoch_floors: BTreeMap<Version, Epoch>,
    }

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct PullResponse {
        pub chunks: Vec<Chunk>,
        /// Where the next page starts; `None` once the holder is drained.
        pub next: Option<SyncPointId>,
    }

    /// One page of the asker's own heads: "do you cover all of these?"
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct VerifyRequest {
        pub scope: Scope,
        pub heads: Vec<(Version, Epoch)>,
    }

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct VerifyResponse {
        pub covered: bool,
    }
}

/// Stored alongside the `SyncPoint`, this is used to store meta information that could provide useful insights later.
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct SyncMeta {
    /// Used to determine the previous write point for a given scope.
    pub parent: Option<SyncPointId>,
    pub parent_epoch: Option<Epoch>,

    pub marker: SyncMarker,

    pub retention_period: Option<Duration>,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub enum SyncMarker {
    Mutation,
    Deletion,
}
