local z = import 'zenoh.libsonnet';

z.router()
+ z.plugins.load({
  db: {},
  introspection: {},
  test_control: {},
})
