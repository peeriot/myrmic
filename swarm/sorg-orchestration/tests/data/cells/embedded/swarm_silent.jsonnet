local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

// Same topology as swarm.jsonnet, with a short init_timeout for the tests
// whose runtime never answers: they would otherwise wait out the 15 s default.
z.peer()
+ z.plugins.dev({
  orchestration: {
    init_timeout_secs: 2
  },
  db: {},
})
