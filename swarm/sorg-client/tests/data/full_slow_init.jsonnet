local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer()
+ z.plugins.dev({
  orchestration: {
    init_timeout_secs: 20
  },
  execution: {},
  db: {},
})
