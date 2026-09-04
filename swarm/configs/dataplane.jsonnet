local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer()
+ z.plugins.dev({
  db: {},
})
+ s.telemetry.logs.env_filter('info,sorg_execution::wasm::host_functions::logging=info')
