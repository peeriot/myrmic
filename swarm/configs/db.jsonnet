local s = import 'swarm.libsonnet';
local z = import 'zenoh.libsonnet';

  z.router()
  + z.plugins.dev({
      db: {},
  })
  + z.transport.disable_batching()
