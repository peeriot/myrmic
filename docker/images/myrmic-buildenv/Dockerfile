# syntax=docker/dockerfile:1
#
# Edgevance Node build environment
#
# Provides:
#   - Rust nightly with riscv32imc-unknown-none-elf (C3) and
#     riscv32imac-unknown-none-elf (C6) targets
#   - wamrc 2.4.4 AOT compiler (WAMR)
#   - espflash for firmware flashing
#
# Published as: ghcr.io/peeriot/edgevance-node-buildenv
#
# Mount the monorepo at /work and run any script from sdk/signal-layer/scripts/:
#
#   docker run --rm \
#     -v $(pwd):/work \
#     -v edgevance-node-cargo-cache:/cache/cargo \
#     -v $SSH_AUTH_SOCK:/ssh-agent -e SSH_AUTH_SOCK=/ssh-agent \
#     ghcr.io/peeriot/edgevance-node-buildenv \
#     /work/sdk/signal-layer/scripts/build.sh basic-sensors

# ── Stage 1: build wamrc (WAMR 2.4.4 AOT compiler) ───────────────────────────
FROM ubuntu:24.04 AS wamrc-builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        cmake \
        curl \
        llvm-dev \
        ninja-build \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /wamr-src

# WAMR's wamr-compiler CMakeLists.txt expects LLVM to be pre-built inside the
# source tree at core/deps/llvm/build.  On Ubuntu 24.04, LLVM 18 is installed
# at /usr/lib/llvm-18 which contains the cmake config find_package needs.
# A symlink satisfies the check without building LLVM from source.
RUN curl -fsSL \
        https://github.com/bytecodealliance/wasm-micro-runtime/archive/refs/tags/WAMR-2.4.4.tar.gz \
    | tar -xz \
    && mkdir -p wasm-micro-runtime-WAMR-2.4.4/core/deps/llvm \
    && ln -s /usr/lib/llvm-18 wasm-micro-runtime-WAMR-2.4.4/core/deps/llvm/build \
    && cmake \
        -S wasm-micro-runtime-WAMR-2.4.4/wamr-compiler \
        -B build \
        -G Ninja \
        -DCMAKE_BUILD_TYPE=Release \
    && cmake --build build \
    && install -m 755 build/wamrc /usr/local/bin/wamrc

# ── Stage 2: main build environment ──────────────────────────────────────────
FROM ubuntu:24.04

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        curl \
        git \
        libclang-dev \
        libssl-dev \
        llvm \
        openssh-client \
        pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && ssh-keyscan github.com >> /etc/ssh/ssh_known_hosts 2>/dev/null

COPY --from=wamrc-builder /usr/local/bin/wamrc /usr/local/bin/wamrc

# ── Rust toolchain (system-wide) ──────────────────────────────────────────────
# RUSTUP_HOME and the initial CARGO_HOME are baked into the image so that
# cargo, rustup, and espflash are always present regardless of what is
# mounted at /cache/cargo at runtime.
ENV RUSTUP_HOME=/opt/rustup
ENV CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain none \
    && rustup toolchain install nightly \
        --target riscv32imc-unknown-none-elf,riscv32imac-unknown-none-elf \
        --component rust-src \
    && rustup default nightly

RUN cargo install --locked --version 4.3.0 espflash

# ── Runtime Cargo cache ───────────────────────────────────────────────────────
# Redirect Cargo's registry/git cache to a mountable path so that downloaded
# crates persist across container runs.  Binaries in /opt/cargo/bin remain
# accessible via PATH regardless of this override.
ENV CARGO_HOME=/cache/cargo

# Use the system git binary so SSH agent forwarding (SSH_AUTH_SOCK) is
# respected for private repositories.
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

WORKDIR /work
