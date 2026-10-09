local z = import "zenoh.libsonnet";

z.router()
+ z.plugins.dev({
  db: {
    load_from: [{ path: "/nonexistent/myrmic-startup-supervision" }],
  },
})
