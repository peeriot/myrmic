local s = import 'swarm.libsonnet';
local z = import 'zenoh.libsonnet';

z.peer('7728d1a01a04f41e7b9e0ff3bab594a2')
+ z.plugins.dev({
  orchestration: {},
  execution: {},
  db: {},
})
+ s.telemetry.logs.pretty()
+ s.telemetry.logs.env_filter('INFO')
// for immediate testing without storage
+ s.telemetry.traces.opentelemetry_export()
