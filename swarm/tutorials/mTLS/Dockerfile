FROM ubuntu:24.04

# Install Dependencies

RUN     apt-get update \
    &&  apt-get install -y \
            curl \
            gettext-base \
            htop \
            iproute2 \
            libdbus-1-3 \
            openssl \
    &&  rm -rf /var/lib/apt/lists/*


# Copy Artifacts

COPY    artifacts/swarm                 /usr/local/bin/
COPY    artifacts/libzenoh_plugin_*.so  /usr/local/lib/
COPY    config/*                        /etc/swarm/
COPY    certs/*                         /etc/swarm/certs/
COPY    scripts/entrypoint              /entrypoint


# Setup Service

RUN     mkdir -p "/var/lib/swarm/filestore"

ENTRYPOINT [ "/entrypoint" ]
