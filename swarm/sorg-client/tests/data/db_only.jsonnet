local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer()
+ z.plugins.dev({
  db: {},
})
