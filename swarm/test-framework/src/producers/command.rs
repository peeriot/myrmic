use std::sync::Arc;
use std::time::SystemTime;

use cell_protocol::Sri;
use rand::Rng;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::{
    clients::sorg::SorgHandle,
    producers::{LoadConfig, SelectionStrategy},
};

pub struct LoadProducer {
    sorg: SorgHandle,
    config: LoadConfig,
}

impl LoadProducer {
    pub fn new(sorg: SorgHandle, config: LoadConfig) -> Self {
        assert!(
            !config.targets.is_empty(),
            "load producer needs at least one target"
        );
        assert!(
            config.rate > 0 && config.rate <= 1_000_000_000,
            "load producer rate must be in 1..=1_000_000_000 commands/sec, got {}",
            config.rate
        );
        Self { sorg, config }
    }

    /// Sends the configured command every `1s / rate`, picking a target each tick according to
    /// `strategy`, for exactly as many ticks as `rate * timeout` implies. Returns the time each
    /// command was dispatched alongside the trace id generated for it.
    ///
    /// Ticking a fixed, precomputed count rather than racing the send interval against a
    /// wall-clock timeout avoids a tie: when `rate * interval == timeout` exactly, the last send
    /// tick and the timeout tick become ready in the same instant, and `select!` would pick
    /// between them arbitrarily.
    pub async fn produce(&self) -> Vec<(SystemTime, Uuid)> {
        let mut in_flight = JoinSet::new();

        let send_interval = std::time::Duration::from_nanos(1_000_000_000 / self.config.rate);
        let mut interval = tokio::time::interval(send_interval);
        interval.tick().await;

        let total_sends = u64::try_from(
            u128::from(self.config.rate) * self.config.timeout.as_nanos() / 1_000_000_000,
        )
        .expect("total send count fits in u64");

        // Parsed once, not once per tick: `target.sri` never changes for the life of the
        // producer, so re-parsing it on every one of up to `rate * timeout` sends would be
        // wasted work on this hot loop.
        let target_sris: Vec<Sri> = self
            .config
            .targets
            .iter()
            .map(|target| Sri::of_path(&target.sri).expect("invalid cell sri"))
            .collect();
        // Shared once via `Arc`, not cloned byte-for-byte on every tick: `cmd_name` never
        // changes for the life of the producer, so each spawned send only needs to bump a
        // refcount rather than copy the string.
        let cmd_name: Arc<str> = Arc::from(self.config.cmd_name.as_str());
        let mut next_target = 0usize;

        for n in 0..total_sends {
            interval.tick().await;

            let index = match self.config.strategy {
                SelectionStrategy::RoundRobin => {
                    let index = next_target;
                    next_target = (next_target + 1) % self.config.targets.len();
                    index
                }
                SelectionStrategy::Random => rand::rng().random_range(0..self.config.targets.len()),
            };
            let target = &self.config.targets[index];
            let sri = target_sris[index];

            let sorg = self.sorg.clone();
            let cmd_name = Arc::clone(&cmd_name);
            let payload = match &self.config.payload_fn {
                Some(payload_fn) => Some(payload_fn(n)),
                None => target.payload.clone(),
            };
            in_flight.spawn(async move {
                let sent_at = SystemTime::now();
                let trace_id = sorg.command_send_traced(sri, &cmd_name, payload).await;
                (sent_at, trace_id)
            });
        }

        let mut produced = Vec::with_capacity(in_flight.len());
        while let Some(result) = in_flight.join_next().await {
            produced.push(result.expect("load producer command task panicked"));
        }
        produced
    }
}
