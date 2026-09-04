# Secure Swarm Test Environment

This setup launches a swarm-based test environment using Docker Compose with prebuilt artifacts and locally created certificates to enable mutual TLS between all nodes.

mTLS can use end-entity certificates containing either domain names or IP addresses in the Subject Alternative Name (SAN) field. If a domain name is used, the listen statement must match that name; otherwise, use the corresponding IP address. Be aware that `0.0.0.0` and `[::]` bindings can update dynamically (for example, due to IPv6 privacy extensions), while the certificate values remain static. Using domain names also requires a functional DNS mechanism in addition to HELO/SCOUT.

You can also use variables inside the configuration templates, such as `${HOSTNAME}`, to dynamically adapt settings for different nodes or environments.

## Prerequisites

- Docker & Docker Compose installed
- Rust and Cargo installed (for building the needed artifacts)

## Build & Prepare

Use the following steps to build the artifacts and prepare certs:

```bash
# Build the swarm artifacts (instead of building them you can also pre-build artifacts)
export TARGET="x86_64-unknown-linux-gnu"
../../.ci/build/swarm

# Copy the artifacts into our current docker environment
./scripts/copy-artifacts

# Create the needed certificates (this is normally done by a CA service somewhere in the network)
./scripts/create-certs
```

## REST Plugin (Optional)

To use the REST plugin to extract data from the spawned swarm nodes, you need to build the plugin inside the (zenoh project)[https://github.com/peeriot/zenoh]:

```bash
# Build the plugin inside the zenoh project directory
cargo build --release -p zenoh-plugin-rest

# Copy the compiled plugin to our artifacts directory (paths may not match)
cp target/release/libzenoh_plugin_rest.so <swarm-repo>/swarm/tutorials/mTLS/artifacts
```

> ⚠️ Note:
>
> Be sure that the Zenoh and Rust versions match the target environment. A mismatch will prevent the plugin from loading.

## Network Structure

We use a structured network with multiple sub-domain certificate and sub-networks:

- **Sub-Domain `swarm.peeriot.intra`**
  - 1x Router A
  - 1x Router B

  - **Sub-Domain `mesh-a.swarm.peeriot.intra`**
    - 2x Peer A (with router connection)
    - 10x Peer A (without router connection)
    - 2x Client A

  - **Sub-Domain `mesh-b.swarm.peeriot.intra`**
    - 2x Peer B (with router connection)
    - 8x Peer B (without router connection)
    - 2x Client B

Each subnetwork and node uses a certificate signed by its corresponding CA, allowing for secure and isolated trust domains with mTLS enforced.

## Start the Environment

Launch the setup using Docker Compose:

```bash
docker compose up -d --force-recreate --build
```

This will bring up all swarm nodes with proper certificates, configuration, and optional plugins if included.

## Extract Meta Information

You can use different scripts to extract meta information from the spawned network:

```bash
# List all IP addresses currently used by the different containers
./scripts/list-ip-addresses

# Extract the logs of all containers and store them into `extracted/logs`
./scripts/extract-logs

# Talk to the REST API of the different nodes to extract the current topology of the network
# The data is stored in `extracted/topology`
./scripts/extract-topology
```
