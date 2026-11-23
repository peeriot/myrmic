local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

// Orchestrator + DB only (no execution): execution runtimes are added per test,
// either as the embedded mock or a linux exec peer.
z.peer()
+ z.plugins.dev({
  orchestration: {},
  db: {},
})
