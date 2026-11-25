local z = import "zenoh.libsonnet";

// A single node carrying every role, with a fixed id so a restart comes back
// as the same runtime. The db directory is supplied per test.
z.peer('b7e2c4a6d8f0b7e2c4a6d8f0b7e2c4a6')
+ z.plugins.dev({
  orchestration: {},
  execution: {},
  db: {},
})
