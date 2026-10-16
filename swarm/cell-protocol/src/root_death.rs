use myrmic_common::cells::LostReason;
use serde::{Deserialize, Serialize};

use crate::{Gen, Sri};

/// A root that died on a live node, and why. `gen_id` is the dead instance's
/// generation, so the orchestrator can tell an uncleaned corpse from a root
/// already restarted at a newer generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootDeath {
    pub sri: Sri,
    pub gen_id: Gen,
    pub reason: LostReason,
}
