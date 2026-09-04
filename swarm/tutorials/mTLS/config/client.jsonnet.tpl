local z = import "zenoh.libsonnet";

[
    z.client()
    + z.plugins.load({
        db: {
            directory: "/var/lib/swarm/filestore"
        }
    })
    + {
        scouting: {
            multicast: {
                enabled: true,
                autoconnect: [ "peer" ]
            },
            gossip: {
                enabled: false
            }
        },
        transport: {
            unicast: {
                max_sessions: 2
            },
            multicast: {
                max_sessions: 2
            },
            link: {
                tls: {
                    enable_mtls: true,
                    verify_name_on_connect: true,
                    root_ca_certificate: "/etc/swarm/certs/root-ca-cert.pem",
                    listen_private_key: "/etc/swarm/certs/client-key.pem",
                    listen_certificate: "/etc/swarm/certs/client-cert-chain.pem",
                    connect_private_key: "/etc/swarm/certs/client-key.pem",
                    connect_certificate: "/etc/swarm/certs/client-cert-chain.pem"
                }
            }
        },
        timestamping: {
            enabled: true
        }
    }
]
