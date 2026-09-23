{
  local this = self,
  local z = function(config) {
    zenoh+: config,
  },

  local defaults = timestamping.all(),

  mode(mode, id=null)::
  {
      mode: mode,
      [if id != null then 'id']: id,
  } + defaults,

  peer(id=null):: z(this.mode('peer', id)),

  router(id=null, port=null)::
    z(this.mode('router', id))
        + if port != null then this.listen.port(port) else {},

  client(id=null):: z(this.mode('client', id)),

  // Unwrapped fragments: they only belong below `zenoh+:`, which `mode()` puts
  // them under. Local rather than exported, because applying them to a config
  // document directly lands a `timestamping` key at the root, where
  // `SwarmConfig` denies it and the node refuses to start.
  local timestamping = {
    local mode(mode) = {
      timestamping+: {
        enabled+: {
          [mode]: true,
        },
      },
    },

    all():: {}
    + self.router()
    + self.peer()
    + self.client(),

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

  // The name this node announces for its own north region. A router upstream
  // assigns it to a south subregion by matching it, see `gateway.south_regions`.
  region(name):: z({ region_name: name }),

  // How a node finds its neighbours. Both merge, so a config can set one, the
  // other, or both; whatever is left unset keeps zenoh's default.
  scouting: {
    multicast(enabled):: z({
      scouting+: {
        multicast+: { enabled: enabled },
      },
    }),
    gossip(enabled, multihop=null, autoconnect=null):: z({
      scouting+: {
        gossip+: {
          enabled: enabled,
          [if multihop != null then 'multihop']: multihop,
          [if autoconnect != null then 'autoconnect']: autoconnect,
        },
      },
    }),
  },

  gateway: {
    // One south subregion per region name, in the order given.
    //
    // A router does not route between two nodes in the same subregion - they
    // are assumed to reach each other directly - so peers that cannot must be
    // told apart. The `auto` preset puts every peer of a router in one
    // subregion, which is right until a router bridges networks.
    south_regions(names):: z({
      gateway: {
        south: [{ filters: [{ region_names: [name] }] } for name in names],
      },
    }),
    // Subregions spelled out, for filters other than a region name (`modes`,
    // `interfaces`, `zids`).
    south(subregions):: z({
      gateway: {
        south: subregions,
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
