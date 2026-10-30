# Image for the containers of the docker swarm backend: runtimes, and the test-side container
# with the CLI and the zenoh router.
#
# The binaries come from the host's build (stripped of debug info), so the base needs a glibc at
# least as new as the build host's. Arch is rolling, and the upgrade below brings it to the newest
# at the time the layer is built.
#
# Needed besides the binaries: `sh`, `mktemp`, `rm` and `printf` for the myrmic shim, all in
# `base`.
FROM archlinux:base

RUN pacman -Syu --noconfirm && pacman -Scc --noconfirm

COPY myrmic zenoh-router /usr/local/bin/
