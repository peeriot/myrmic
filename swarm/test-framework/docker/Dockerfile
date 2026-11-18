# Image for the containers of the docker swarm backend: runtimes, and the container running the
# zenoh router.
#
# The binaries come from the host's build (stripped of debug info), so the base needs a glibc at
# least as new as the build host's. Arch is rolling, and the upgrade below brings it to the newest
# at the time the layer is built.
#
# Needed besides the binaries: `sh`, `mktemp`, `rm` and `printf` for the myrmic shim, all in
# `base`, `nft` for isolating a runtime, and libfaketime for skewing its clock.
FROM archlinux:base

RUN pacman -Syu --noconfirm nftables libfaketime && pacman -Scc --noconfirm

# Every process in a container reads its wall clock through libfaketime, at the offset from the
# host's clock that FAKETIME_TIMESTAMP_FILE holds, in seconds. Docker can't move one container's
# wall clock otherwise: all containers share the host kernel's. The monotonic clock stays real, as
# with a real clock step, and the file is read again at most a second after it changes.
ENV LD_PRELOAD=/usr/lib/faketime/libfaketimeMT.so.1 \
    FAKETIME_TIMESTAMP_FILE=/etc/faketime \
    FAKETIME_DONT_FAKE_MONOTONIC=1 \
    FAKETIME_CACHE_DURATION=1
RUN echo +0 > /etc/faketime

COPY myrmic zenoh-router /usr/local/bin/
