use std::{sync::Arc, time::Duration};

pub use fjall::{RemoteTx, Store, Transaction};

pub mod fjall;

#[derive(Clone, Default)]
pub struct Options {
    /// If provided, the data will be persisted and stored here.
    /// Otherwise, it will use a tmp directory which will be cleaned up on drop.
    pub directory: Option<std::path::PathBuf>,

    /// Used to coordinate transactions and replication processes.
    pub logic_clock: Arc<uhlc::HLC>,

    /// How often the GC task scans for expired data. Defaults to 60 seconds.
    pub gc_interval: Option<Duration>,
}

#[cfg(test)]
impl Options {
    pub fn test() -> Self {
        let clock = uhlc::HLCBuilder::new().with_clock(uhlc::zero_clock).build();

        Self {
            directory: None,
            logic_clock: Arc::new(clock),
            gc_interval: None,
        }
    }
}

pub type TransactionId = uuid::Uuid;

#[derive(Copy, Clone)]
pub enum TransactionMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Copy, Clone)]
pub struct TransactionOptions {
    pub mode: TransactionMode,

    pub retention_period: Option<Duration>,

    /// Aborts the transaction if it sits unused for this long.
    /// Enforced for remote transactions by the store's reaper.
    pub idle_timeout: Option<Duration>,
}

impl TransactionOptions {
    pub fn write() -> Self {
        Self::new(TransactionMode::ReadWrite)
    }

    pub fn read() -> Self {
        Self::new(TransactionMode::ReadOnly)
    }

    pub fn new(mode: TransactionMode) -> Self {
        Self {
            mode,
            retention_period: None,
            idle_timeout: None,
        }
    }

    pub fn retain_for(mode: TransactionMode, retention_period: Duration) -> Self {
        Self {
            mode,
            retention_period: Some(retention_period),
            idle_timeout: None,
        }
    }
}
