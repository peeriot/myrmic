local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer('7728d1a01a04f41e7b9e0ff3bab594a2')
+ z.plugins.dev({
  orchestration: {},
  execution: {
    mailbox_poll_interval_ms: 50,
    },
  db: {},
})
