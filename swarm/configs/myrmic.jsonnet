local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer()
+ s.telemetry.logs.compact()
+ s.telemetry.logs.env_filter('debug,h2=warn,sorg_execution=warn,sorg_common=warn,db=warn,db_client=warn,wasmtime=off,cranelift_codegen=off,zenoh=off,swarm_telemetry=off,opentelemetry_sdk=off,hyper_util=off,rustls=off')
+ z.plugins.dev({
  db: {},
  orchestration: {},
  execution: {},
  mqtt: {},
})
