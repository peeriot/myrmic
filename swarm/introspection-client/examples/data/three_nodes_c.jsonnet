local z = import "zenoh.libsonnet";

z.peer('7a28d1a01a04f41e7b9e0ff3bab594a2')
+ z.plugins.dev({
  execution: {},
})
