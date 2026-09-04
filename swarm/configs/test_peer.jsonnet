local z = import 'zenoh.libsonnet';

z.peer()
+ {
  zenoh+: {
    scouting: {
      multicast: { enabled: true },
      gossip: { enabled: false },
    },
  },
}
+ z.timestamping.all()
+ z.plugins.load({
  db: {},
  introspection: {},
  orchestration: {},
  execution: {},
  test_control: {},
})
