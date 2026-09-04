local z = import "zenoh.libsonnet";

z.peer()
+ z.plugins.load({
  orchestration: {},
  execution: {},
})
