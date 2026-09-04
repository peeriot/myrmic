local z = import "zenoh.libsonnet";

z.peer()
  + z.plugins.dev({
    execution: {
        name: 'runtime 2',
        tags: ['tag_two']
    },
  })
