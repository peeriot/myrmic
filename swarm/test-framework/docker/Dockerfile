# Image for the hosts of `host_set::Docker`: the containers that runtimes and zenoh routers run
# in.
#
# The binaries come from the host's build (stripped of debug info), so the base needs a glibc at
# least as new as the build host's. Arch is rolling, and the upgrade below brings it to the newest
# at the time the layer is built.
#
# Needed besides the binaries: `sh`, `mktemp`, `rm` and `printf` for the myrmic shim, and `ip` and
# `tc` for moving a host and degrading its links, all in `base`; `nft` for blocking and counting a
# host's traffic, `ethtool` for making its interface handle packets one by one, and libfaketime
# for skewing its clock.
FROM archlinux:base

RUN pacman -Syu --noconfirm nftables ethtool libfaketime && pacman -Scc --noconfirm

# Every process in a container reads its wall clock through libfaketime, at the offset from the
# host's clock that FAKETIME_TIMESTAMP_FILE holds, in seconds. Docker can't move one container's
# wall clock otherwise: all containers share the host kernel's. The monotonic clock stays real, as
# with a real clock step, and the file is read again at most a second after it changes.
#
# Without FAKETIME_DISABLE_SHM, the first process that loads libfaketime creates a semaphore and
# shared memory in /dev/shm, its children inherit them, and the creator removes them when it exits.
# A daemonized runtime outlives the `runtimes start` that created them, and libfaketime ends it
# with status 1 at its next intercepted `stat` call. Nothing here needs that state: it serves
# counters shared between processes (FAKETIME_START_AFTER_*), not the offset file.
ENV LD_PRELOAD=/usr/lib/faketime/libfaketimeMT.so.1 \
    FAKETIME_TIMESTAMP_FILE=/etc/faketime \
    FAKETIME_DONT_FAKE_MONOTONIC=1 \
    FAKETIME_CACHE_DURATION=1 \
    FAKETIME_DISABLE_SHM=1
RUN echo +0 > /etc/faketime

COPY myrmic zenoh-router /usr/local/bin/
