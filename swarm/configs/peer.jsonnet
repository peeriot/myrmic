local z = import "zenoh.libsonnet";

z.router()
+ z.plugins.load({
  orchestration: {},
  execution: {},
})
