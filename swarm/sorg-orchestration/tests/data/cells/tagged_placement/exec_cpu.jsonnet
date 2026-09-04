local z = import "zenoh.libsonnet";

z.peer()
+ z.plugins.dev({
  execution: {
    name: 'RT_CPU',
    tags: ['cpu'],
  },
})
