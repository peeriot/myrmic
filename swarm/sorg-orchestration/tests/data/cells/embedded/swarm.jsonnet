local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

// Orchestrator + DB only (no execution): execution runtimes are added per test,
// either as the embedded mock or a linux exec peer. A short init_timeout keeps
// the silent-deploy test from hanging on the confirmation deadline.
z.peer()
+ z.plugins.dev({
  orchestration: {
    init_timeout_secs: 2
  },
  db: {},
})
