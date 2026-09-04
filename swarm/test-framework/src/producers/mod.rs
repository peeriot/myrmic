use std::time::Duration;

pub mod command;

/// One command variant a [`command::LoadProducer`] can dispatch: which cell to call and which
/// payload to send it.
pub struct LoadTarget {
    /// SRI of the cell to send commands to
    pub sri: String,
    /// payload attached to each command
    pub payload: Option<Vec<u8>>,
}

/// How a [`command::LoadProducer`] picks the next [`LoadTarget`] on each tick.
pub enum SelectionStrategy {
    /// cycle through `targets` in order, wrapping back to the start
    RoundRobin,
    /// pick a target uniformly at random on every tick
    Random,
}

pub struct LoadConfig {
    /// name of the command to send
    pub cmd_name: String,
    /// candidate targets, picked from according to `strategy` on every tick
    pub targets: Vec<LoadTarget>,
    /// how to pick a target from `targets` on each tick
    pub strategy: SelectionStrategy,
    /// aggregate commands/sec sent across all targets combined
    pub rate: u64,
    /// total duration to keep producing commands for
    pub timeout: Duration,
    /// when set, computes each send's payload from its running send index (0..total sends),
    /// overriding the picked target's own `payload`. Lets a caller vary the payload independently
    /// of which target `strategy` selected — e.g. picking a second, unrelated value (like a zone
    /// id) on its own round-robin/random cycle every send.
    pub payload_fn: Option<Box<dyn Fn(u64) -> Vec<u8> + Send + Sync>>,
}
