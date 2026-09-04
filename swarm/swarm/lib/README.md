This folder just contains the jsonnet "API" for the swarm.

`default.libsonnet` holds the "default" configuration from the view of zenoh (at the time of last modification)

`swarm.libsonnet` will eventually hold configuration functions for configuring a given "node" outside of zenoh.
(Something like env control, links, etc)

`zenoh.libsonnet` is used to build the zenoh config.
It contains things like helper functions and, broadly speaking, an opinionated config.

