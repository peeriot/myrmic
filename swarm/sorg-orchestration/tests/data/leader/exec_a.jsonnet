local z = import "zenoh.libsonnet";

z.peer('37c72f467bc9c77f41b73fe16f054741')
+ z.plugins.dev({
  execution: {}
})
