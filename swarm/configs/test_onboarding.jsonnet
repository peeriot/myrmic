local z = import "zenoh.libsonnet";

z.peer()
+ z.plugins.dev({
  orchestration: {},
  execution: {},
  onboarding: {},
})
+ {
  zenoh+: {
    listen: {
      endpoints: { router: ['tcp/[::]:7447'], peer: ['tcp/[::]:47447'], 'bt_gatt/hci0' },
    },
  },
}
