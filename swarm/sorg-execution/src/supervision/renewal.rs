use std::time::Duration;

use cell_protocol::node_tags::LiveTags;
use cell_protocol::{NodeLease, RuntimeId};
use sorg_common::node_lease;
use sorg_common::supervision::{SupervisionTiming, jittered};
use tracing::warn;
use zenoh::Session;

/// Slow cadence (in renewal ticks) for healing a missing exec registry row.
const REGISTRY_HEAL_EVERY: u64 = 6;

pub(crate) fn next_renewal_delay(timing: &SupervisionTiming, tick: u64) -> Duration {
    jittered(timing.renew, tick)
}

/// Renews this node's liveness lease forever. Failures are logged and retried
/// next tick — the lease going stale on persistent failure IS the designed
/// signal, not an error path to handle.
pub(crate) fn spawn_renewal(
    session: Session,
    id: RuntimeId,
    device_id: String,
    timing: SupervisionTiming,
    name: Option<String>,
    tags: LiveTags,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let retention = timing.lease_retention();
        let ttl_ms = u64::try_from(timing.ttl.as_millis()).unwrap_or(u64::MAX);
        let mut tick: u64 = 0;
        loop {
            tick += 1;
            // Wall-clock millis, not a counter: node ids are stable across
            // restarts, so the seq must keep advancing through one — a
            // restarted counter would look like a frozen (dead) lease to
            // observers that saw the old incarnation's higher values.
            let seq = u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            )
            .unwrap_or(u64::MAX);
            let lease = NodeLease {
                device_id: device_id.clone(),
                seq,
                ttl_ms,
            };
            if let Err(err) = node_lease::renew_lease(&session, id, &lease, retention).await {
                warn!("lease renewal {seq} failed: {err}");
            }

            // The registry row should live as long as this exec, but a stale
            // leave-deregistration racing a restart (or hygiene during a lease
            // outage) can delete it, and boot-time registration never re-runs.
            // Built from the live tags, so this also repairs a row left behind
            // by a retag that failed to publish. Heal on a slow cadence.
            if tick.is_multiple_of(REGISTRY_HEAL_EVERY) {
                let info = crate::spawn::runtime_info(&session, name.clone(), &tags);

                match sorg_common::exec_registry::ensure_registered(&session, &info).await {
                    Ok(true) => warn!("exec registry row was missing or stale; re-registered"),
                    Ok(false) => {}
                    Err(err) => warn!("exec registry heal failed: {err}"),
                }
            }

            tokio::time::sleep(next_renewal_delay(&timing, tick)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewal_delays_are_jittered_per_tick_and_bounded() {
        let timing = SupervisionTiming::default();
        let d1 = next_renewal_delay(&timing, 1);
        let d2 = next_renewal_delay(&timing, 2);
        assert_ne!(d1, d2);
        assert!(d1 >= Duration::from_secs(8) && d1 <= Duration::from_secs(12));
        assert!(d2 >= Duration::from_secs(8) && d2 <= Duration::from_secs(12));
    }
}
