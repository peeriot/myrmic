local z = import "zenoh.libsonnet";

// A plain execution runtime. It registers with the `linux` capability tag
// (added automatically by the execution runtime), so an `embedded`-tagged cell
// will never be placed here — it serves as a distractor in routing tests.
z.peer()
+ z.plugins.dev({
  execution: {
    name: 'RT_LINUX',
  },
})
