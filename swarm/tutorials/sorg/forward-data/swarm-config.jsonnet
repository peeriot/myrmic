local z = import "zenoh.libsonnet";

z.peer()
  + z.plugins.dev({
    orchestration: {},
    execution: {
        name: 'runtime',
    }
  })
