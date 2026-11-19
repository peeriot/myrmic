local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer('7728d1a01a04f41e7b9e0ff3bab594a2')
+ z.plugins.dev({
  orchestration: {},
  // 64 KiB: below the 128 KiB of initial memory the runtime-tags fixture declares.
  execution: { cell_memory: 65536 },
  db: {},
})
