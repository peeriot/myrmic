local z = import "zenoh.libsonnet";

# The Peer ID needs to be the same as what's used by sorg-tests or the health-check will fail
z.peer('7728d1a01a04f41e7b9e0ff3bab594a2')
+ z.plugins.dev({
    # Embedded deployment (download + flash) takes well over the 15 s default,
    # so give the orchestrator enough time to receive the device's confirmation.
    orchestration: { init_timeout_secs: 120 },
    # Allows for deploying cells on Linux too
    execution: {},
    # DB for storing cells and cell states
    db: {},
}) + {
 zenoh+: {
   listen: {
     endpoints: { router: ['tcp/[::]:7447'], peer: ['tcp/[::]:7447'], },
   },
 },
}
