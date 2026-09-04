local z = import "zenoh.libsonnet";

z.peer('2cc8a35064c529faaa1924134d13e2ad')
+ z.plugins.dev({
  orchestration: {},
  db: {},
})
