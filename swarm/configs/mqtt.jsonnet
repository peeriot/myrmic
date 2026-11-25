local s = import 'swarm.libsonnet';
local z = import 'zenoh.libsonnet';

  z.router()
  + z.plugins.dev({
      mqtt: {
          allow: [
              "building/*"
          ]
      },
  })
