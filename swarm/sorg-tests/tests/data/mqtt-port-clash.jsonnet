local z = import "zenoh.libsonnet";

local listener(name) = {
  name: name,
  listen: "127.0.0.1:18831",
  next_connection_delay_ms: 1,
  connections: {
    connection_timeout_ms: 60000,
    max_payload_size: 20480,
    max_inflight_count: 100,
    dynamic_filters: true,
  },
};

z.router()
+ z.plugins.dev({
  mqtt: {
    v4: [listener("v4-clash")],
    v5: [listener("v5-clash")],
  },
})
