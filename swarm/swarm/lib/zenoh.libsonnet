{
  local this = self,
  local z = function(config) {
    zenoh+: config,
  },

  defaults: this.timestamping.all(),

  mode(mode, id=null)::
  {
      mode: mode,
      [if id != null then 'id']: id,
  } + this.defaults,

  peer(id=null):: z(this.mode('peer', id)),

  router(id=null, port=null)::
    z(this.mode('router', id))
        + if port != null then this.listen.port(port) else {},

  client(id=null):: z(this.mode('client', id)),

  timestamping: {
    local mode(mode) = {
      timestamping+: {
        enabled+: {
          [mode]: true,
        },
      },
    },

    all():: {}
    + this.timestamping.router()
    + this.timestamping.peer()
    + this.timestamping.client(),

    router():: mode('router'),
    peer():: mode('peer'),
    client():: mode('client'),
  },

  listen: {
    port(port):: z({
      listen: {
        endpoints: {
          router: ['tcp/[::]:' + port],
          peer: ['tcp/[::]:0'],
        },
      },
    }),
    endpoints(endpoints):: z({
      listen: {
        endpoints: endpoints,
      },
    }),
  },

  transport: {
    disable_batching():: z({
         transport+: {
             link+: {
                 tx+: {
                     queue+: {
                         batching+: {
                             enabled: false,
                         },
                     }
                 }
             }
         }
     })
  },

  plugin(name, config):: {
      [name]: config,
  },

  plugins: {
    with(plugin_configs):: plugin_configs,
    dev(plugin_configs):: plugin_configs,
    load(config):: config,
  },
}
