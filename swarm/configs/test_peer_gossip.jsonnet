local z = import 'zenoh.libsonnet';

z.peer()
+ {
  zenoh+: {
    scouting: {
      gossip: {
        enabled: true,
        multihop: false,
        autoconnect: {
            router: [],
            peer: ["router", "peer"]
        },
      },
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
