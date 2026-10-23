# Build environment for yabridge (tools/build-yabridge.sh). Same base as the Wine build image
# (build-support/wine/Dockerfile); the Wine itself is mounted in from our own build, so yabridge is
# compiled and linked against the Wine it runs under.
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential meson ninja-build pkg-config cmake git ca-certificates python3 \
        libxcb1-dev libdbus-1-dev \
    && rm -rf /var/lib/apt/lists/*
