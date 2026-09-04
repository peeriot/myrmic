local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.router()
+ {
  zenoh+: {
    listen: {
      endpoints:  [
        "bt_gatt/Peeriot.Framework"
      ]
    },
    transport: {
      unicast: {
        accept_timeout: 30000
      }
    }
  }
}
