local s = import 'swarm.libsonnet';
local z = import 'zenoh.libsonnet';

  z.peer()
  + z.plugins.dev({
      mqtt: {
          allow: [
              "building/*"
          ]
      },
  })
