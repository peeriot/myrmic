local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer()
+ z.plugins.dev({
  orchestration: {},
  execution: { mailbox_batch_size: 32, event_buffer_size: 64 },
  db: {},
})
+ s.telemetry.logs.env_filter('info')
// Without this, "no retention configured" means telemetry (logs/traces/metrics) never gets
// written to the DB at all — and the benchmark harness measures latency/completeness by
// querying trace spans back out of that DB, so a run would silently produce empty results.
+ { telemetry+: { db_retention: '1h' } }
