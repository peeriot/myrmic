//! Zenoh module for Swarm Onboarding

use embassy_time::Duration;

pub mod device;
pub mod installer;

mod io;

/// Retry timeout when querying an onboarding topic
const QUERY_RETRY_TIMEOUT: Duration = Duration::from_secs(3);
