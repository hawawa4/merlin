# Build environment for our patched Wine (tools/build-wine.sh).
FROM ubuntu:25.10
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64 \
        gcc-mingw-w64-i686 binutils-mingw-w64-i686 flex bison gettext \
        pkg-config python3 xz-utils ca-certificates \
        libx11-dev libxext-dev libxrandr-dev libxi-dev libxcursor-dev libxinerama-dev \
        libxcomposite-dev libxfixes-dev libxrender-dev libxxf86vm-dev libxkbcommon-dev \
        libfreetype-dev libfontconfig-dev libasound2-dev libpulse-dev libgnutls28-dev \
        libvulkan-dev libudev-dev libsdl2-dev libusb-1.0-0-dev libpcap-dev libunwind-dev \
        libdbus-1-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    && rm -rf /var/lib/apt/lists/*
