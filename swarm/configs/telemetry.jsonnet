local s = import 'swarm.libsonnet';
local z = import 'zenoh.libsonnet';

z.peer()
+ z.plugins.dev({
  db: {},
  orchestration: {},
  execution: {},
})
+ z.transport.disable_batching()
+ s.telemetry.logs.pretty()
+ s.telemetry.opentelemetry_export('http://localhost:4317')
