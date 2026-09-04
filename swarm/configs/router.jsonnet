local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.router()
+ z.plugins.load({
  db: s.db.load_from('../../target', max_depth = 1)
})
