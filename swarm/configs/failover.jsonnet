local z = import "zenoh.libsonnet";

z.router()
+ z.plugins.dev({
    orchestration: {},
    execution: {}
  })
+ {
    zenoh+: {
      transport: {
        link: {
          tx: {
            lease: 1000 // lease length in milliseconds; smaller lease = faster error detection
          }
        }
      }
    }
  }
