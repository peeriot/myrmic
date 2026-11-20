local z = import "zenoh.libsonnet";
local s = import "swarm.libsonnet";

z.peer('7728d1a01a04f41e7b9e0ff3bab594a2')
+ z.plugins.dev({
  orchestration: {},
  // runner_fuel is small enough that one runaway attempt ends within
  // milliseconds. cell_memory is 6 pages (384 KiB), a few above the 2 pages the
  // limits fixture starts with.
  execution: { runner_fuel: 10000000, cell_memory: 393216 },
  db: {},
})
