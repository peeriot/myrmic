local z = import 'zenoh.libsonnet';

z.peer()
+ {
  scouting: {
    multicast: { enabled: false },
    gossip: { enabled: false },
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
