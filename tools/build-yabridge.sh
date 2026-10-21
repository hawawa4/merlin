#!/usr/bin/env bash
# Builds yabridge at a pinned commit, plus our patches (build-support/yabridge-patches/), in its
# upstream CI image (no host build deps needed).
# Output: runtime/yabridge-<commit>-patched/ with the plugin libraries and Wine host binaries.
set -euo pipefail

COMMIT="${1:-b580a9f7}"   # master, 2026-08-02: Wine 10/11 embedding + cursor-offset fixes
IMAGE="robbert/docker-yabridge:focal"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/runtime/src/yabridge"
PATCHES="$ROOT/build-support/yabridge-patches"
OUT="$ROOT/runtime/yabridge-$COMMIT-patched"

if [ -e "$OUT/libyabridge-clap.so" ]; then
    echo "already built: $OUT"
    exit 0
fi

mkdir -p "$ROOT/runtime/src"
[ -d "$SRC/.git" ] || git clone https://github.com/robbert-vdh/yabridge.git "$SRC"
git -C "$SRC" fetch --quiet origin
git -C "$SRC" checkout --quiet "$COMMIT"
git -C "$SRC" checkout --quiet -- src   # drop any previous patching
for p in "$PATCHES"/*.patch; do
    echo "applying $(basename "$p")"
    git -C "$SRC" apply "$p"
done
rm -rf "$SRC/build"

docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -e WINEPREFIX=/tmp/prefix \
    -v "$SRC:/src" -w /src "$IMAGE" bash -c '
        set -e
        git config --global --add safe.directory /src
        meson setup build --buildtype=release --cross-file=cross-wine.conf \
            --unity=on --unity-size=10000 -Dbitbridge=false
        ninja -C build
        strip build/libyabridge{,-chainloader}-{clap,vst2,vst3}.so build/yabridge-host.exe.so'

mkdir -p "$OUT"
cp "$SRC"/build/libyabridge{,-chainloader}-{clap,vst2,vst3}.so \
   "$SRC"/build/yabridge-host.exe{,.so} "$OUT/"
echo "$COMMIT" > "$OUT/COMMIT"
echo "built: $OUT"
ls -la "$OUT"
