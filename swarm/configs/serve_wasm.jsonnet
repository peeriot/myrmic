local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer()
+ z.plugins.dev({
  db: s.db.load_from('./target/wasm32-wasip1/debug', max_depth = 1),
})
