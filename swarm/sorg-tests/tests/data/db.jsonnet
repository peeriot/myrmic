local z = import "zenoh.libsonnet";

z.router()
+ z.plugins.dev({
  db: {},
})
