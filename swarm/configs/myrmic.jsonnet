local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer()
+ s.telemetry.logs.compact()
// zenoh stays off apart from the two modules that report a node moving to a new address
+ s.telemetry.logs.env_filter('debug,h2=warn,sorg_execution=warn,sorg_common=warn,db=warn,db_client=warn,wasmtime=off,cranelift_codegen=off,zenoh=off,zenoh::net::runtime::interface_monitor=debug,zenoh::net::runtime::orchestrator=info,swarm_telemetry=off,opentelemetry_sdk=off,hyper_util=off,rustls=off')
+ z.plugins.dev({
  db: {},
  orchestration: {},
  execution: {},
  mqtt: {},
})
