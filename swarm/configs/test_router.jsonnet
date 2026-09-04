local s = import 'swarm.libsonnet';
local z = import 'zenoh.libsonnet';

z.router()
+ z.timestamping.all()
+ z.plugins.load({
  swarm_sem_store: {},
  db: {},
  introspection: {},
  test_control: {},
})
