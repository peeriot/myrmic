FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    iproute2 \
    iptables \
    && rm -rf /var/lib/apt/lists/*

COPY swarm /usr/local/bin/swarm
COPY test_router.jsonnet /etc/peeriot/swarm/test_router.jsonnet
COPY test_peer.jsonnet /etc/peeriot/swarm/test_peer.jsonnet
COPY test_peer_gossip.jsonnet /etc/peeriot/swarm/test_peer_gossip.jsonnet
COPY test_peer_no_scouting.jsonnet /etc/peeriot/swarm/test_peer_no_scouting.jsonnet
